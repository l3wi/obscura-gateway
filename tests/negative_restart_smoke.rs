use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const LIVE_SMOKE_ENV: &str = "OBSCURA_LIVE_SMOKE";
const KEEP_STATE_ENV: &str = "OBSCURA_LIVE_SMOKE_KEEP_STATE";

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
struct SessionRecord {
    session_id: String,
    state: String,
    profile_id: Option<String>,
    proxy_policy: String,
}

#[derive(Debug, Deserialize)]
struct NavigateResponse {
    url: String,
    ready_state: String,
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
    total_sessions: usize,
    active_sessions: usize,
}

struct TestServer {
    home: Option<TempDir>,
    state_root: PathBuf,
    logs_dir: PathBuf,
    client: Client,
    listen_addr: String,
    base_url: String,
    api_key: String,
    child: Child,
    launch_index: usize,
}

impl TestServer {
    async fn start() -> Result<Self> {
        let home = tempfile::tempdir().context("failed to create temp home")?;
        let state_root = home.path().join(".obscura-gateway");
        let logs_dir = home.path().join("logs");
        fs::create_dir_all(&logs_dir)?;

        run_gateway_cli(
            home.path(),
            ["setup"].as_slice(),
            logs_dir.join("setup.stdout.log"),
            logs_dir.join("setup.stderr.log"),
        )
        .await?;

        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build reqwest client")?;

        let mut server = Self {
            home: Some(home),
            state_root,
            logs_dir,
            client,
            listen_addr: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            child: Command::new("true")
                .spawn()
                .context("failed to create placeholder child")?,
            launch_index: 0,
        };
        server.restart().await?;
        Ok(server)
    }

    async fn restart(&mut self) -> Result<()> {
        if self.launch_index > 0 {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }

        let home = self
            .home
            .as_ref()
            .ok_or_else(|| anyhow!("test home missing"))?;
        let listen_addr = format!("127.0.0.1:{}", pick_free_port()?);
        let base_url = format!("http://{listen_addr}");
        rewrite_server_settings(
            &self.state_root.join("config.toml"),
            &listen_addr,
            &base_url,
        )?;

        let config = load_config(&self.state_root.join("config.toml"))?;
        if config.listen_addr != listen_addr {
            bail!(
                "unexpected listen_addr in config: {} != {}",
                config.listen_addr,
                listen_addr
            );
        }
        if config.server_url != base_url {
            bail!(
                "unexpected server_url in config: {} != {}",
                config.server_url,
                base_url
            );
        }

        let stdout_file = fs::File::create(
            self.logs_dir
                .join(format!("server-{}.stdout.log", self.launch_index)),
        )?;
        let stderr_file = fs::File::create(
            self.logs_dir
                .join(format!("server-{}.stderr.log", self.launch_index)),
        )?;
        let mut child = Command::new(gateway_bin())
            .arg("run")
            .env("HOME", home.path())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .spawn()
            .context("failed to start obscura-gateway server")?;

        let mut last_err = None;
        for _ in 0..60 {
            if let Some(status) = child.try_wait()? {
                bail!(
                    "obscura-gateway exited early with status {status}; see {}",
                    self.logs_dir.display()
                );
            }
            match self.client.get(format!("{base_url}/healthz")).send().await {
                Ok(response) if response.status().is_success() => {
                    self.listen_addr = listen_addr;
                    self.base_url = base_url;
                    self.api_key = config.api_key;
                    self.child = child;
                    self.launch_index += 1;
                    return Ok(());
                }
                Ok(response) => last_err = Some(anyhow!("healthz status {}", response.status())),
                Err(err) => last_err = Some(err.into()),
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        let _ = child.kill().await;
        Err(last_err.unwrap_or_else(|| anyhow!("gateway did not become healthy after restart")))
    }

    fn auth(&self, request: RequestBuilder) -> RequestBuilder {
        request.bearer_auth(&self.api_key)
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

    async fn delete_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        self.auth(self.client.delete(format!("{}{}", self.base_url, path)))
            .send()
            .await?
            .error_for_status()?
            .json::<T>()
            .await
            .map_err(Into::into)
    }

    async fn shutdown(mut self) -> Result<()> {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        if std::env::var_os(KEEP_STATE_ENV).is_some() {
            if let Some(home) = self.home.take() {
                let persist = home.keep();
                eprintln!("preserved negative smoke state at {}", persist.display());
            }
        }
        Ok(())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        if std::env::var_os(KEEP_STATE_ENV).is_some() {
            eprintln!(
                "negative smoke logs preserved at {}",
                self.logs_dir.display()
            );
        }
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

fn rewrite_server_settings(path: &Path, listen_addr: &str, base_url: &str) -> Result<()> {
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
    table.insert(
        "server_url".to_string(),
        toml::Value::String(base_url.to_string()),
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

async fn create_session(server: &TestServer) -> Result<SessionRecord> {
    server
        .post_json(
            "/v1/sessions",
            json!({
                "tenant_id": null,
                "profile_id": null,
                "profile_mode": null,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_negative_failures_without_proxy_dependency() -> Result<()> {
    if !live_smoke_enabled() {
        eprintln!("skipping live smoke test; set {LIVE_SMOKE_ENV}=1 to enable");
        return Ok(());
    }

    let _guard = smoke_lock().lock().await;
    let server = TestServer::start().await?;

    let unauthorized = server
        .client
        .get(format!("{}/v1/status", server.base_url))
        .send()
        .await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let missing_profile = server
        .auth(
            server
                .client
                .get(format!("{}/v1/profiles/does-not-exist", server.base_url)),
        )
        .send()
        .await?;
    assert_eq!(missing_profile.status(), StatusCode::NOT_FOUND);

    let missing_profile_session = server
        .auth(
            server
                .client
                .post(format!("{}/v1/sessions", server.base_url)),
        )
        .json(&json!({
            "tenant_id": null,
            "profile_id": "does-not-exist",
            "profile_mode": "read_only",
            "allowed_domains": [],
            "denied_domains": [],
            "proxy_policy": "direct",
        }))
        .send()
        .await?;
    assert_eq!(missing_profile_session.status(), StatusCode::BAD_REQUEST);

    let unknown_proxy_policy = server
        .auth(
            server
                .client
                .post(format!("{}/v1/sessions", server.base_url)),
        )
        .json(&json!({
            "tenant_id": null,
            "profile_id": null,
            "profile_mode": null,
            "allowed_domains": [],
            "denied_domains": [],
            "proxy_policy": "definitely-not-configured",
        }))
        .send()
        .await?;
    assert_eq!(unknown_proxy_policy.status(), StatusCode::BAD_REQUEST);

    let bad_session_lookup = server
        .auth(
            server
                .client
                .get(format!("{}/v1/sessions/not-a-uuid", server.base_url)),
        )
        .send()
        .await?;
    assert_eq!(bad_session_lookup.status(), StatusCode::NOT_FOUND);

    let bad_session_eval = server
        .auth(server.client.post(format!(
            "{}/v1/sessions/not-a-uuid/actions/eval",
            server.base_url
        )))
        .json(&json!({ "expression": "1 + 1" }))
        .send()
        .await?;
    assert_eq!(bad_session_eval.status(), StatusCode::BAD_REQUEST);

    server.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_restart_marks_direct_session_failed() -> Result<()> {
    if !live_smoke_enabled() {
        eprintln!("skipping live smoke test; set {LIVE_SMOKE_ENV}=1 to enable");
        return Ok(());
    }

    let _guard = smoke_lock().lock().await;
    let mut server = TestServer::start().await?;

    let session = create_session(&server).await?;
    assert_eq!(session.proxy_policy, "direct");
    assert_eq!(session.profile_id, None);

    let nav = navigate(&server, &session.session_id, "https://example.com/").await?;
    assert_eq!(nav.url, "https://example.com/");
    assert_eq!(nav.ready_state, "complete");

    let before_restart: StatusResponse = server.get_json("/v1/status").await?;
    assert_eq!(before_restart.total_sessions, 1);
    assert_eq!(before_restart.active_sessions, 1);

    server.restart().await?;

    let persisted: SessionRecord = server
        .get_json(&format!("/v1/sessions/{}", session.session_id))
        .await?;
    assert_eq!(persisted.session_id, session.session_id);
    assert_eq!(persisted.proxy_policy, "direct");
    assert_eq!(persisted.state, "failed");

    let after_restart: StatusResponse = server.get_json("/v1/status").await?;
    assert_eq!(after_restart.total_sessions, 1);
    assert_eq!(after_restart.active_sessions, 0);

    let stale_eval = server
        .auth(server.client.post(format!(
            "{}/v1/sessions/{}/actions/eval",
            server.base_url, session.session_id
        )))
        .json(&json!({ "expression": "document.title" }))
        .send()
        .await?;
    assert_eq!(stale_eval.status(), StatusCode::BAD_REQUEST);

    let closed: SessionRecord = server
        .delete_json(&format!("/v1/sessions/{}", session.session_id))
        .await?;
    assert_eq!(closed.state, "closed");

    let final_status: StatusResponse = server.get_json("/v1/status").await?;
    assert_eq!(final_status.total_sessions, 1);
    assert_eq!(final_status.active_sessions, 0);

    server.shutdown().await
}
