use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use reqwest::multipart::{Form, Part};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const LIVE_SMOKE_ENV: &str = "OBSCURA_LIVE_SMOKE";

#[derive(Debug, Deserialize)]
struct GatewayConfig {
    api_key: String,
    listen_addr: String,
    server_url: String,
}

#[derive(Debug, Deserialize)]
struct ProfileRecord {
    profile_id: String,
    cookie_count: usize,
}

#[derive(Debug, Deserialize)]
struct SessionRecord {
    session_id: String,
    state: String,
    proxy_policy: String,
}

#[derive(Debug, Deserialize)]
struct NavigateResponse {
    url: String,
    ready_state: String,
}

#[derive(Debug, Deserialize)]
struct EvaluateResponse {
    value: Value,
}

#[derive(Debug, Deserialize)]
struct GrantResponse {
    ws_url: String,
}

#[derive(Debug, Deserialize)]
struct GatewayEvent {
    kind: String,
}

struct TestServer {
    _home: TempDir,
    state_root: PathBuf,
    client: Client,
    base_url: String,
    api_key: String,
    child: Child,
}

struct EventStream {
    rx: mpsc::Receiver<GatewayEvent>,
}

impl TestServer {
    async fn start_direct() -> Result<Self> {
        let home = tempfile::tempdir().context("failed to create temp home")?;
        let state_root = home.path().join(".obscura-gateway");
        let logs_dir = home.path().join("logs");
        fs::create_dir_all(&logs_dir)?;

        let listen_addr = format!("127.0.0.1:{}", pick_free_port()?);
        let base_url = format!("http://{listen_addr}");

        run_gateway_cli(
            home.path(),
            &["setup"],
            logs_dir.join("setup.stdout.log"),
            logs_dir.join("setup.stderr.log"),
        )
        .await?;
        run_gateway_cli(
            home.path(),
            &["config", "set-server-url", &base_url],
            logs_dir.join("config-server-url.stdout.log"),
            logs_dir.join("config-server-url.stderr.log"),
        )
        .await?;
        rewrite_listen_addr(&state_root.join("config.toml"), &listen_addr)?;

        let config = load_config(&state_root.join("config.toml"))?;
        if config.server_url != base_url {
            bail!(
                "unexpected server_url in config: {} != {}",
                config.server_url,
                base_url
            );
        }
        if config.listen_addr != listen_addr {
            bail!(
                "unexpected listen_addr in config: {} != {}",
                config.listen_addr,
                listen_addr
            );
        }

        let stdout_file = fs::File::create(logs_dir.join("server.stdout.log"))?;
        let stderr_file = fs::File::create(logs_dir.join("server.stderr.log"))?;
        let mut child = Command::new(gateway_bin())
            .arg("run")
            .env("HOME", home.path())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .spawn()
            .context("failed to start obscura-gateway server")?;

        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .context("failed to build reqwest client")?;

        let mut last_err = None;
        for _ in 0..60 {
            if let Some(status) = child.try_wait()? {
                bail!("obscura-gateway exited early with status {status}");
            }
            match client.get(format!("{base_url}/healthz")).send().await {
                Ok(response) if response.status().is_success() => {
                    return Ok(Self {
                        _home: home,
                        state_root,
                        client,
                        base_url,
                        api_key: config.api_key,
                        child,
                    });
                }
                Ok(response) => last_err = Some(anyhow!("healthz status {}", response.status())),
                Err(err) => last_err = Some(err.into()),
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        let _ = child.kill().await;
        Err(last_err.unwrap_or_else(|| anyhow!("gateway did not become healthy")))
    }

    fn auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.api_key)
    }

    async fn post_json<T: for<'de> Deserialize<'de>>(&self, path: &str, body: Value) -> Result<T> {
        self.auth(self.client.post(format!("{}{}", self.base_url, path)))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<T>()
            .await
            .map_err(Into::into)
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        self.auth(self.client.get(format!("{}{}", self.base_url, path)))
            .send()
            .await?
            .error_for_status()?
            .json::<T>()
            .await
            .map_err(Into::into)
    }

    async fn delete_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        self.auth(self.client.delete(format!("{}{}", self.base_url, path)))
            .send()
            .await?
            .error_for_status()?
            .json::<T>()
            .await
            .map_err(Into::into)
    }

    async fn import_cookies(
        &self,
        profile_id: &str,
        file_name: &str,
        contents: String,
    ) -> Result<Value> {
        let form = Form::new().part(
            "file",
            Part::bytes(contents.into_bytes()).file_name(file_name.to_string()),
        );
        self.auth(self.client.post(format!(
            "{}/v1/profiles/{profile_id}/cookies:import",
            self.base_url
        )))
        .multipart(form)
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await
        .map_err(Into::into)
    }

    async fn subscribe_events(&self, session_id: &str) -> Result<EventStream> {
        let response = self
            .auth(
                self.client
                    .get(format!("{}/v1/sessions/{session_id}/events", self.base_url)),
            )
            .send()
            .await?
            .error_for_status()?;
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(async move {
            let mut response = response;
            let mut buffer = String::new();
            while let Ok(chunk) = response.chunk().await {
                let Some(chunk) = chunk else { break };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buffer.find("\n\n") {
                    let frame = buffer[..pos].to_string();
                    buffer.drain(..pos + 2);
                    let data = frame
                        .lines()
                        .filter_map(|line| line.strip_prefix("data: "))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if data.is_empty() {
                        continue;
                    }
                    if let Ok(event) = serde_json::from_str::<GatewayEvent>(&data) {
                        if tx.send(event).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(EventStream { rx })
    }

    async fn shutdown(mut self) -> Result<()> {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        Ok(())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl EventStream {
    async fn expect_kinds(&mut self, expected: &[&str]) -> Result<Vec<String>> {
        let mut actual = Vec::with_capacity(expected.len());
        for kind in expected {
            let event = timeout(Duration::from_secs(20), self.rx.recv())
                .await
                .context("timed out waiting for SSE event")?
                .ok_or_else(|| anyhow!("event stream closed before receiving {kind}"))?;
            actual.push(event.kind.clone());
            if event.kind != *kind {
                bail!("expected SSE event {kind}, got {}", event.kind);
            }
        }
        Ok(actual)
    }
}

fn live_smoke_enabled() -> bool {
    matches!(std::env::var(LIVE_SMOKE_ENV).as_deref(), Ok("1"))
}

fn gateway_bin() -> &'static str {
    env!("CARGO_BIN_EXE_obscura-gateway")
}

fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn load_config(path: &Path) -> Result<GatewayConfig> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    toml::from_str(&raw).context("failed to parse gateway config")
}

fn rewrite_listen_addr(path: &Path, listen_addr: &str) -> Result<()> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let mut value: toml::Value = toml::from_str(&raw).context("failed to parse config TOML")?;
    let table = value
        .as_table_mut()
        .ok_or_else(|| anyhow!("gateway config was not a TOML table"))?;
    table.insert(
        "listen_addr".to_string(),
        toml::Value::String(listen_addr.to_string()),
    );
    fs::write(path, toml::to_string_pretty(&value)?)
        .with_context(|| format!("failed to write config file {}", path.display()))?;
    Ok(())
}

async fn run_gateway_cli(
    home: &Path,
    args: &[&str],
    stdout_path: PathBuf,
    stderr_path: PathBuf,
) -> Result<()> {
    let output = Command::new(gateway_bin())
        .args(args)
        .env("HOME", home)
        .output()
        .await
        .with_context(|| format!("failed to execute {:?} {:?}", gateway_bin(), args))?;
    fs::write(&stdout_path, &output.stdout)?;
    fs::write(&stderr_path, &output.stderr)?;
    if !output.status.success() {
        bail!(
            "command {:?} {:?} failed with {}\nstdout:\n{}\nstderr:\n{}",
            gateway_bin(),
            args,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    Ok(())
}

async fn create_profile(server: &TestServer, name: &str) -> Result<ProfileRecord> {
    server
        .post_json(
            "/v1/profiles",
            json!({
                "name": name,
                "description": "smoke profile",
                "identity": {},
            }),
        )
        .await
}

async fn create_session(server: &TestServer, profile_id: Option<&str>) -> Result<SessionRecord> {
    server
        .post_json(
            "/v1/sessions",
            json!({
                "tenant_id": null,
                "profile_id": profile_id,
                "profile_mode": "read_only",
                "allowed_domains": [],
                "denied_domains": [],
                "proxy_policy": "direct",
            }),
        )
        .await
}

async fn navigate(server: &TestServer, session_id: &str, url: &str) -> Result<NavigateResponse> {
    server
        .post_json(
            &format!("/v1/sessions/{session_id}/actions/navigate"),
            json!({
                "url": url,
                "wait_until": "load",
                "timeout_secs": 45,
            }),
        )
        .await
}

async fn evaluate(server: &TestServer, session_id: &str, expression: &str) -> Result<Value> {
    let response: EvaluateResponse = server
        .post_json(
            &format!("/v1/sessions/{session_id}/actions/eval"),
            json!({ "expression": expression }),
        )
        .await?;
    Ok(response.value)
}

async fn dump(server: &TestServer, session_id: &str) -> Result<Value> {
    server
        .post_json(
            &format!("/v1/sessions/{session_id}/actions/dump"),
            json!({ "format": "text" }),
        )
        .await
}

async fn close_session(server: &TestServer, session_id: &str) -> Result<SessionRecord> {
    server
        .delete_json(&format!("/v1/sessions/{session_id}"))
        .await
}

async fn mint_grant(server: &TestServer, session_id: &str) -> Result<GrantResponse> {
    server
        .post_json(&format!("/v1/sessions/{session_id}/grants/cdp"), json!({}))
        .await
}

async fn connect_grant_and_eval(ws_url: &str) -> Result<String> {
    let ws_url = ws_url
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let (mut stream, _) = connect_async(ws_url).await?;

    let create_result = cdp_request(
        &mut stream,
        1,
        "Target.createTarget",
        Some(json!({ "url": "about:blank" })),
        None,
    )
    .await?;
    let target_id = create_result
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("grant target create missing targetId"))?
        .to_string();

    let attach_result = cdp_request(
        &mut stream,
        2,
        "Target.attachToTarget",
        Some(json!({ "targetId": target_id, "flatten": true })),
        None,
    )
    .await?;
    let session_id = attach_result
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("grant attach missing sessionId"))?
        .to_string();

    let _ = cdp_request(&mut stream, 3, "Runtime.enable", None, Some(&session_id)).await?;
    let eval = cdp_request(
        &mut stream,
        4,
        "Runtime.evaluate",
        Some(json!({ "expression": "'grant-ok'" })),
        Some(&session_id),
    )
    .await?;

    eval.get("result")
        .and_then(|value| value.get("value"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("grant evaluation did not return a string result"))
}

async fn cdp_request(
    stream: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: i64,
    method: &str,
    params: Option<Value>,
    session_id: Option<&str>,
) -> Result<Value> {
    let mut payload = json!({
        "id": id,
        "method": method,
    });
    if let Some(params) = params {
        payload["params"] = params;
    }
    if let Some(session_id) = session_id {
        payload["sessionId"] = Value::String(session_id.to_string());
    }
    stream
        .send(Message::Text(payload.to_string().into()))
        .await?;
    loop {
        let message = timeout(Duration::from_secs(20), stream.next())
            .await
            .context("timed out waiting for CDP response")?
            .ok_or_else(|| anyhow!("CDP stream closed before response"))??;
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text)?;
            if value.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(error) = value.get("error") {
                    bail!("CDP {} returned error: {}", method, error);
                }
                return Ok(value
                    .get("result")
                    .cloned()
                    .ok_or_else(|| anyhow!("CDP {} missing result", method))?);
            }
        }
    }
}

fn session_event_kinds(state_root: &Path, session_id: &str) -> Result<Vec<String>> {
    let connection = Connection::open(state_root.join("gateway.db"))
        .context("failed to open smoke gateway database")?;
    let mut stmt = connection
        .prepare("select kind from session_events where session_id = ?1 order by rowid asc")?;
    let rows = stmt.query_map([session_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_direct_events_and_grant_single_use_without_proxy() -> Result<()> {
    if !live_smoke_enabled() {
        eprintln!("skipping live smoke test; set {LIVE_SMOKE_ENV}=1 to enable");
        return Ok(());
    }

    let server = TestServer::start_direct().await?;
    let session = create_session(&server, None).await?;
    assert_eq!(session.proxy_policy, "direct");

    assert_eq!(
        session_event_kinds(&server.state_root, &session.session_id)?,
        vec!["session.created".to_string()]
    );

    let mut events = server.subscribe_events(&session.session_id).await?;

    let nav = navigate(&server, &session.session_id, "https://example.com/").await?;
    assert_eq!(nav.ready_state, "complete");
    assert_eq!(nav.url, "https://example.com/");

    let title = evaluate(&server, &session.session_id, "document.title").await?;
    assert_eq!(title, json!("Example Domain"));

    let dumped = dump(&server, &session.session_id).await?;
    assert_eq!(dumped["format"], "text");
    assert!(
        dumped["content"]
            .as_str()
            .unwrap_or_default()
            .contains("Example Domain")
    );

    let grant = mint_grant(&server, &session.session_id).await?;
    assert_eq!(connect_grant_and_eval(&grant.ws_url).await?, "grant-ok");
    assert!(
        connect_grant_and_eval(&grant.ws_url).await.is_err(),
        "expected grant reuse to fail"
    );

    let closed = close_session(&server, &session.session_id).await?;
    assert_eq!(closed.state, "closed");

    let live_kinds = events
        .expect_kinds(&[
            "session.navigate",
            "session.eval",
            "session.dump",
            "cdp.attached",
            "session.closed",
        ])
        .await?;
    assert_eq!(
        live_kinds,
        vec![
            "session.navigate".to_string(),
            "session.eval".to_string(),
            "session.dump".to_string(),
            "cdp.attached".to_string(),
            "session.closed".to_string(),
        ]
    );

    assert_eq!(
        session_event_kinds(&server.state_root, &session.session_id)?,
        vec![
            "session.created".to_string(),
            "session.navigate".to_string(),
            "session.eval".to_string(),
            "session.dump".to_string(),
            "cdp.attached".to_string(),
            "session.closed".to_string(),
        ]
    );

    let artifacts: Vec<Value> = server
        .get_json(&format!("/v1/sessions/{}/artifacts", session.session_id))
        .await?;
    assert!(
        artifacts.is_empty(),
        "expected direct session smoke flow to leave no session artifacts"
    );

    server.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_cookie_import_artifact_exists_while_session_artifacts_stay_empty() -> Result<()> {
    if !live_smoke_enabled() {
        eprintln!("skipping live smoke test; set {LIVE_SMOKE_ENV}=1 to enable");
        return Ok(());
    }

    let server = TestServer::start_direct().await?;
    let profile = create_profile(&server, "cookie-import-smoke").await?;
    assert_eq!(profile.cookie_count, 0);

    let seed_cookie =
        "# Netscape HTTP Cookie File\n.example.com\tTRUE\t/\tFALSE\t2147483647\tsmoke_cookie\t1\n";
    let import_response = server
        .import_cookies(
            &profile.profile_id,
            "seed-cookies.txt",
            seed_cookie.to_string(),
        )
        .await?;
    assert_eq!(import_response["profile_id"], profile.profile_id);
    assert!(import_response["imported"].as_u64().unwrap_or_default() >= 1);

    let import_artifact = server
        .state_root
        .join("profiles")
        .join(&profile.profile_id)
        .join("last-cookie-import");
    let imported_contents = fs::read_to_string(&import_artifact)
        .with_context(|| format!("expected import artifact at {}", import_artifact.display()))?;
    assert!(imported_contents.contains("smoke_cookie"));

    let session = create_session(&server, Some(&profile.profile_id)).await?;
    assert_eq!(session.proxy_policy, "direct");

    let nav = navigate(&server, &session.session_id, "https://example.com/").await?;
    assert_eq!(nav.ready_state, "complete");

    let artifacts: Vec<Value> = server
        .get_json(&format!("/v1/sessions/{}/artifacts", session.session_id))
        .await?;
    assert!(
        artifacts.is_empty(),
        "expected cookie import to stay in the profile artifact only"
    );

    let closed = close_session(&server, &session.session_id).await?;
    assert_eq!(closed.state, "closed");

    server.shutdown().await
}
