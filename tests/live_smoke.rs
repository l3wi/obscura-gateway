use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const LIVE_SMOKE_ENV: &str = "OBSCURA_LIVE_SMOKE";
const KEEP_STATE_ENV: &str = "OBSCURA_LIVE_SMOKE_KEEP_STATE";
const PROXY_HOST_ENV: &str = "OBSCURA_PROXY_BRIDGE_HOST";
const PROXY_POLICY_NAME: &str = "camofox_ch";

fn smoke_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Deserialize)]
struct GatewayConfig {
    api_key: String,
    listen_addr: String,
    server_url: String,
}

#[derive(Debug, Deserialize)]
struct ProfileRecord {
    profile_id: String,
    description: String,
    cookie_count: usize,
}

#[derive(Debug, Deserialize)]
struct SessionRecord {
    session_id: String,
    state: String,
    profile_id: Option<String>,
    proxy_policy: String,
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
    saved_profiles: usize,
    total_sessions: usize,
    active_sessions: usize,
}

#[derive(Debug, Deserialize)]
struct QuotasResponse {
    active_sessions: usize,
    profiles: usize,
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
    home: Option<TempDir>,
    state_root: PathBuf,
    logs_dir: PathBuf,
    client: Client,
    base_url: String,
    api_key: String,
    child: Child,
}

struct EventStream {
    rx: mpsc::Receiver<GatewayEvent>,
}

impl TestServer {
    async fn start() -> Result<Self> {
        let home = tempfile::tempdir().context("failed to create temp home")?;
        let state_root = home.path().join(".obscura-gateway");
        let logs_dir = home.path().join("logs");
        fs::create_dir_all(&logs_dir)?;

        let listen_addr = format!("127.0.0.1:{}", pick_free_port()?);
        let base_url = format!("http://{listen_addr}");

        run_gateway_cli(
            home.path(),
            ["setup"].as_slice(),
            logs_dir.join("setup.stdout.log"),
            logs_dir.join("setup.stderr.log"),
        )
        .await?;
        run_gateway_cli(
            home.path(),
            ["config", "set-server-url", &base_url].as_slice(),
            logs_dir.join("config-server-url.stdout.log"),
            logs_dir.join("config-server-url.stderr.log"),
        )
        .await?;
        rewrite_listen_addr(&state_root.join("config.toml"), &listen_addr)?;

        let proxy = load_camofox_proxy().await?;
        run_gateway_cli(
            home.path(),
            [
                "config",
                "upsert-proxy-policy",
                PROXY_POLICY_NAME,
                &proxy.scheme,
                &proxy.host,
                &proxy.port.to_string(),
                "--country",
                &proxy.country,
                "--city",
                &proxy.city,
            ]
            .as_slice(),
            logs_dir.join("config-proxy.stdout.log"),
            logs_dir.join("config-proxy.stderr.log"),
        )
        .await?;

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
                bail!(
                    "obscura-gateway exited early with status {status}; see {}",
                    logs_dir.display()
                );
            }
            match client.get(format!("{base_url}/healthz")).send().await {
                Ok(response) if response.status().is_success() => {
                    return Ok(Self {
                        home: Some(home),
                        state_root,
                        logs_dir,
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

    async fn patch_json<T: for<'de> Deserialize<'de>>(&self, path: &str, body: Value) -> Result<T> {
        self.auth(self.client.patch(format!("{}{}", self.base_url, path)))
            .json(&body)
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

    async fn delete_empty(&self, path: &str) -> Result<()> {
        self.auth(self.client.delete(format!("{}{}", self.base_url, path)))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn get_text(&self, path: &str) -> Result<String> {
        self.auth(self.client.get(format!("{}{}", self.base_url, path)))
            .send()
            .await?
            .error_for_status()?
            .text()
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
        if std::env::var_os(KEEP_STATE_ENV).is_some() {
            if let Some(home) = self.home.take() {
                let persist = home.keep();
                eprintln!("preserved live smoke state at {}", persist.display());
            }
        }
        Ok(())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        if std::env::var_os(KEEP_STATE_ENV).is_some() {
            eprintln!("live smoke logs preserved at {}", self.logs_dir.display());
        }
    }
}

impl EventStream {
    async fn expect_kind(&mut self, expected: &str) -> Result<()> {
        let event = timeout(Duration::from_secs(20), self.rx.recv())
            .await
            .context("timed out waiting for SSE event")?
            .ok_or_else(|| anyhow!("event stream closed before receiving {expected}"))?;
        if event.kind != expected {
            bail!("expected SSE event {expected}, got {}", event.kind);
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ProxySettings {
    scheme: String,
    host: String,
    port: u16,
    country: String,
    city: String,
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

async fn load_camofox_proxy() -> Result<ProxySettings> {
    let raw = fs::read_to_string("/root/dev/camofox-browser/.env")
        .context("failed to read /root/dev/camofox-browser/.env")?;
    let mut values = std::collections::HashMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            values.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }
    let host = std::env::var(PROXY_HOST_ENV).unwrap_or_else(|_| "172.19.0.2".to_string());
    Ok(ProxySettings {
        scheme: values
            .get("PROXY_SCHEME")
            .cloned()
            .unwrap_or_else(|| "socks5".to_string()),
        host,
        port: values
            .get("PROXY_PORT")
            .ok_or_else(|| anyhow!("PROXY_PORT missing from camofox env"))?
            .parse()
            .context("invalid PROXY_PORT in camofox env")?,
        country: values
            .get("PROXY_COUNTRY")
            .cloned()
            .unwrap_or_else(|| "CH".to_string()),
        city: values
            .get("PROXY_CITY")
            .cloned()
            .unwrap_or_else(|| "Zurich".to_string()),
    })
}

async fn create_profile(
    server: &TestServer,
    name: &str,
    description: &str,
    identity: Value,
) -> Result<ProfileRecord> {
    server
        .post_json(
            "/v1/profiles",
            json!({
                "name": name,
                "description": description,
                "identity": identity,
            }),
        )
        .await
}

async fn create_session(
    server: &TestServer,
    profile_id: Option<&str>,
    profile_mode: Option<&str>,
    proxy_policy: Option<&str>,
) -> Result<SessionRecord> {
    server
        .post_json(
            "/v1/sessions",
            json!({
                "tenant_id": null,
                "profile_id": profile_id,
                "profile_mode": profile_mode,
                "allowed_domains": [],
                "denied_domains": [],
                "proxy_policy": proxy_policy,
            }),
        )
        .await
}

async fn navigate(
    server: &TestServer,
    session_id: &str,
    url: &str,
    wait_until: &str,
) -> Result<NavigateResponse> {
    server
        .post_json(
            &format!("/v1/sessions/{session_id}/actions/navigate"),
            json!({
                "url": url,
                "wait_until": wait_until,
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

async fn dump(server: &TestServer, session_id: &str, format: &str) -> Result<Value> {
    server
        .post_json(
            &format!("/v1/sessions/{session_id}/actions/dump"),
            json!({ "format": format }),
        )
        .await
}

async fn close_session(server: &TestServer, session_id: &str) -> Result<SessionRecord> {
    server
        .delete_json(&format!("/v1/sessions/{session_id}"))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_server_auth_and_profile_crud() -> Result<()> {
    if !live_smoke_enabled() {
        eprintln!("skipping live smoke test; set {LIVE_SMOKE_ENV}=1 to enable");
        return Ok(());
    }
    let _guard = smoke_lock().lock().await;
    let server = TestServer::start().await?;

    let health = server
        .client
        .get(format!("{}/healthz", server.base_url))
        .send()
        .await?;
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(health.json::<Value>().await?["ok"], true);

    let unauth = server
        .client
        .get(format!("{}/v1/status", server.base_url))
        .send()
        .await?;
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let status: StatusResponse = server.get_json("/v1/status").await?;
    assert_eq!(status.saved_profiles, 0);
    assert_eq!(status.total_sessions, 0);
    assert_eq!(status.active_sessions, 0);

    let profile = create_profile(
        &server,
        "crud-profile",
        "profile for CRUD smoke test",
        json!({}),
    )
    .await?;
    assert_eq!(profile.description, "profile for CRUD smoke test");
    assert_eq!(profile.cookie_count, 0);

    let profiles: Vec<ProfileRecord> = server.get_json("/v1/profiles").await?;
    assert_eq!(profiles.len(), 1);

    let fetched: ProfileRecord = server
        .get_json(&format!("/v1/profiles/{}", profile.profile_id))
        .await?;
    assert_eq!(fetched.profile_id, profile.profile_id);

    let updated: ProfileRecord = server
        .patch_json(
            &format!("/v1/profiles/{}", profile.profile_id),
            json!({
                "description": "updated description",
                "identity": {
                    "accept_language": "en-US,en;q=0.9"
                }
            }),
        )
        .await?;
    assert_eq!(updated.description, "updated description");

    let quotas: QuotasResponse = server.get_json("/v1/quotas").await?;
    assert_eq!(quotas.profiles, 1);
    assert_eq!(quotas.active_sessions, 0);

    server
        .delete_empty(&format!("/v1/profiles/{}", profile.profile_id))
        .await?;

    let profiles: Vec<ProfileRecord> = server.get_json("/v1/profiles").await?;
    assert!(profiles.is_empty());
    let status: StatusResponse = server.get_json("/v1/status").await?;
    assert_eq!(status.saved_profiles, 0);

    server.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_single_session_live_navigation_and_dumps() -> Result<()> {
    if !live_smoke_enabled() {
        eprintln!("skipping live smoke test; set {LIVE_SMOKE_ENV}=1 to enable");
        return Ok(());
    }
    let _guard = smoke_lock().lock().await;
    let server = TestServer::start().await?;

    let session = create_session(&server, None, None, Some("direct")).await?;
    assert_eq!(session.proxy_policy, "direct");

    let mut events = server.subscribe_events(&session.session_id).await?;

    let nav = navigate(&server, &session.session_id, "https://example.com/", "load").await?;
    assert_eq!(nav.ready_state, "complete");
    assert_eq!(nav.url, "https://example.com/");
    events.expect_kind("session.navigate").await?;

    let title = evaluate(&server, &session.session_id, "document.title").await?;
    assert_eq!(title, json!("Example Domain"));
    events.expect_kind("session.eval").await?;

    let html = dump(&server, &session.session_id, "html").await?;
    assert_eq!(html["format"], "html");
    assert!(
        html["content"]
            .as_str()
            .unwrap_or_default()
            .contains("Example Domain")
    );
    events.expect_kind("session.dump").await?;

    let text = dump(&server, &session.session_id, "text").await?;
    assert_eq!(text["format"], "text");
    assert!(
        text["content"]
            .as_str()
            .unwrap_or_default()
            .contains("Example Domain")
    );
    events.expect_kind("session.dump").await?;

    let links = dump(&server, &session.session_id, "links").await?;
    assert_eq!(links["format"], "links");
    let has_iana = links["links"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .any(|link| {
            link["url"]
                .as_str()
                .unwrap_or_default()
                .contains("iana.org")
        });
    assert!(has_iana, "expected an IANA link in example.com dump");
    events.expect_kind("session.dump").await?;

    let hn_nav = navigate(
        &server,
        &session.session_id,
        "https://news.ycombinator.com/",
        "domcontentloaded",
    )
    .await?;
    assert!(hn_nav.url.contains("news.ycombinator.com"));
    events.expect_kind("session.navigate").await?;

    let hn_title = evaluate(&server, &session.session_id, "document.title").await?;
    assert_eq!(hn_title, json!("Hacker News"));
    events.expect_kind("session.eval").await?;

    let hn_links = dump(&server, &session.session_id, "links").await?;
    assert!(
        hn_links["links"]
            .as_array()
            .map(|links| links.len())
            .unwrap_or(0)
            > 10,
        "expected multiple links on Hacker News"
    );
    events.expect_kind("session.dump").await?;

    let networkidle = navigate(
        &server,
        &session.session_id,
        "https://example.com/",
        "networkidle",
    )
    .await?;
    assert_eq!(networkidle.ready_state, "complete");
    events.expect_kind("session.navigate").await?;

    let artifacts: Vec<Value> = server
        .get_json(&format!("/v1/sessions/{}/artifacts", session.session_id))
        .await?;
    assert!(artifacts.is_empty(), "expected no session artifacts");

    let closed = close_session(&server, &session.session_id).await?;
    assert_eq!(closed.state, "closed");
    events.expect_kind("session.closed").await?;

    server.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_profile_cookie_round_trip_across_sessions() -> Result<()> {
    if !live_smoke_enabled() {
        eprintln!("skipping live smoke test; set {LIVE_SMOKE_ENV}=1 to enable");
        return Ok(());
    }
    let _guard = smoke_lock().lock().await;
    let server = TestServer::start().await?;

    let profile_a = create_profile(
        &server,
        "cookies-a",
        "source profile for cookie round-trip",
        json!({}),
    )
    .await?;
    let seed_cookie =
        "# Netscape HTTP Cookie File\n.example.com\tTRUE\t/\tFALSE\t2147483647\tsmoke_cookie\t1\n";
    let imported_seed = server
        .import_cookies(
            &profile_a.profile_id,
            "seed-cookies.txt",
            seed_cookie.to_string(),
        )
        .await?;
    assert_eq!(imported_seed["profile_id"], profile_a.profile_id);
    assert!(imported_seed["imported"].as_u64().unwrap_or_default() >= 1);

    let session_a = create_session(
        &server,
        Some(&profile_a.profile_id),
        Some("read_write"),
        Some("direct"),
    )
    .await?;

    let _ = navigate(
        &server,
        &session_a.session_id,
        "https://example.com/",
        "load",
    )
    .await?;

    let closed = close_session(&server, &session_a.session_id).await?;
    assert_eq!(closed.state, "closed");

    let cookies_json = server
        .get_text(&format!(
            "/v1/profiles/{}/cookies:export?format=json",
            profile_a.profile_id
        ))
        .await?;
    assert!(cookies_json.contains("smoke_cookie"));

    let cookies_netscape = server
        .get_text(&format!(
            "/v1/profiles/{}/cookies:export?format=netscape",
            profile_a.profile_id
        ))
        .await?;
    assert!(cookies_netscape.contains("smoke_cookie"));

    let profile_b = create_profile(
        &server,
        "cookies-b",
        "target profile for cookie import",
        json!({}),
    )
    .await?;
    let import_response = server
        .import_cookies(
            &profile_b.profile_id,
            "cookies.txt",
            cookies_netscape.clone(),
        )
        .await?;
    assert_eq!(import_response["profile_id"], profile_b.profile_id);
    assert!(import_response["imported"].as_u64().unwrap_or_default() >= 1);

    let import_artifact = server
        .state_root
        .join("profiles")
        .join(&profile_b.profile_id)
        .join("last-cookie-import");
    let imported_contents = fs::read_to_string(&import_artifact)
        .with_context(|| format!("expected import artifact at {}", import_artifact.display()))?;
    assert!(imported_contents.contains("smoke_cookie"));

    let session_b = create_session(
        &server,
        Some(&profile_b.profile_id),
        Some("read_only"),
        Some("direct"),
    )
    .await?;
    let _ = navigate(
        &server,
        &session_b.session_id,
        "https://example.com/",
        "load",
    )
    .await?;

    let profile_b_fetched: ProfileRecord = server
        .get_json(&format!("/v1/profiles/{}", profile_b.profile_id))
        .await?;
    assert!(profile_b_fetched.cookie_count >= 1);

    let closed = close_session(&server, &session_b.session_id).await?;
    assert_eq!(closed.state, "closed");

    server.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_multi_session_proxy_and_grants() -> Result<()> {
    if !live_smoke_enabled() {
        eprintln!("skipping live smoke test; set {LIVE_SMOKE_ENV}=1 to enable");
        return Ok(());
    }
    let _guard = smoke_lock().lock().await;
    let server = TestServer::start().await?;

    let direct_profile = create_profile(
        &server,
        "direct-profile",
        "direct traffic profile",
        json!({}),
    )
    .await?;
    let proxy_profile = create_profile(
        &server,
        "proxy-profile",
        "proxy traffic profile",
        json!({
            "proxy_affinity": PROXY_POLICY_NAME
        }),
    )
    .await?;
    let shared_profile = create_profile(
        &server,
        "shared-readonly-profile",
        "shared profile for concurrent read-only sessions",
        json!({}),
    )
    .await?;

    let direct = create_session(
        &server,
        Some(&direct_profile.profile_id),
        Some("read_only"),
        Some("direct"),
    )
    .await?;
    let proxied = create_session(
        &server,
        Some(&proxy_profile.profile_id),
        Some("read_only"),
        None,
    )
    .await?;
    let shared_one = create_session(
        &server,
        Some(&shared_profile.profile_id),
        Some("read_only"),
        Some("direct"),
    )
    .await?;
    let shared_two = create_session(
        &server,
        Some(&shared_profile.profile_id),
        Some("read_only"),
        Some("direct"),
    )
    .await?;

    assert_eq!(direct.proxy_policy, "direct");
    assert_eq!(proxied.proxy_policy, PROXY_POLICY_NAME);
    assert_eq!(
        shared_one.profile_id.as_deref(),
        Some(shared_profile.profile_id.as_str())
    );
    assert_eq!(
        shared_two.profile_id.as_deref(),
        Some(shared_profile.profile_id.as_str())
    );

    let _ = navigate(
        &server,
        &direct.session_id,
        "https://api.ipify.org/?format=json",
        "load",
    )
    .await?;

    let direct_ip_raw = evaluate(&server, &direct.session_id, "document.body.innerText").await?;
    let direct_ip: Value = serde_json::from_str(
        direct_ip_raw
            .as_str()
            .ok_or_else(|| anyhow!("direct ifconfig response was not a string"))?,
    )?;
    assert_ne!(direct_ip["ip"], Value::Null);

    let grant: GrantResponse = server
        .post_json(
            &format!("/v1/sessions/{}/grants/cdp", proxied.session_id),
            json!({}),
        )
        .await?;
    let grant_title = connect_grant_and_eval(&grant.ws_url).await?;
    assert_eq!(grant_title, "grant-ok");

    let reused = connect_grant_and_eval(&grant.ws_url).await;
    assert!(reused.is_err(), "expected grant reuse to fail");

    for session in [&direct, &proxied, &shared_one, &shared_two] {
        let closed = close_session(&server, &session.session_id).await?;
        assert_eq!(closed.state, "closed");
    }

    let status: StatusResponse = server.get_json("/v1/status").await?;
    assert_eq!(status.active_sessions, 0);

    server.shutdown().await
}
