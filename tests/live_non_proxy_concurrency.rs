use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const LIVE_SMOKE_ENV: &str = "OBSCURA_LIVE_SMOKE";
const KEEP_STATE_ENV: &str = "OBSCURA_LIVE_SMOKE_KEEP_STATE";
const LOCK_DIR: &str = "/tmp/obscura-gateway-live-smoke.lock";

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
}

#[derive(Debug, Deserialize)]
struct SessionRecord {
    session_id: String,
    state: String,
    profile_id: Option<String>,
    profile_mode: String,
    proxy_policy: String,
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
    total_sessions: usize,
    active_sessions: usize,
}

#[derive(Debug, Deserialize)]
struct NavigateResponse {
    url: String,
    ready_state: String,
}

struct CrossProcessSmokeLock {
    path: &'static str,
}

impl CrossProcessSmokeLock {
    async fn acquire() -> Result<Self> {
        for _ in 0..120 {
            match fs::create_dir(LOCK_DIR) {
                Ok(()) => return Ok(Self { path: LOCK_DIR }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(err) => return Err(err).context("failed to create cross-process smoke lock"),
            }
        }
        bail!("timed out waiting for live smoke lock at {LOCK_DIR}");
    }
}

impl Drop for CrossProcessSmokeLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(self.path);
    }
}

struct TestServer {
    home: Option<TempDir>,
    logs_dir: PathBuf,
    client: Client,
    base_url: String,
    api_key: String,
    child: Child,
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

    async fn create_session_raw(&self, body: Value) -> Result<reqwest::Response> {
        self.auth(self.client.post(format!("{}/v1/sessions", self.base_url)))
            .json(&body)
            .send()
            .await
            .map_err(Into::into)
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

async fn create_profile(
    server: &TestServer,
    name: &str,
    description: &str,
) -> Result<ProfileRecord> {
    server
        .post_json(
            "/v1/profiles",
            json!({
                "name": name,
                "description": description,
                "identity": {},
            }),
        )
        .await
}

async fn create_session(
    server: &TestServer,
    profile_id: Option<&str>,
    profile_mode: Option<&str>,
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

async fn evaluate_title(server: &TestServer, session_id: &str) -> Result<String> {
    let value: Value = server
        .post_json(
            &format!("/v1/sessions/{session_id}/actions/eval"),
            json!({ "expression": "document.title" }),
        )
        .await?;
    value["value"]
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("document.title did not evaluate to a string"))
}

async fn close_session(server: &TestServer, session_id: &str) -> Result<SessionRecord> {
    server
        .auth(
            server
                .client
                .delete(format!("{}/v1/sessions/{session_id}", server.base_url)),
        )
        .send()
        .await?
        .error_for_status()?
        .json::<SessionRecord>()
        .await
        .map_err(Into::into)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_non_proxy_concurrency_and_profile_locking() -> Result<()> {
    if !live_smoke_enabled() {
        eprintln!("skipping live smoke test; set {LIVE_SMOKE_ENV}=1 to enable");
        return Ok(());
    }

    let _guard = smoke_lock().lock().await;
    let _cross_process_guard = CrossProcessSmokeLock::acquire().await?;
    let server = TestServer::start().await?;

    let direct_one = create_session(&server, None, None).await?;
    let direct_two = create_session(&server, None, None).await?;
    let direct_three = create_session(&server, None, None).await?;

    for session in [&direct_one, &direct_two, &direct_three] {
        assert_eq!(session.proxy_policy, "direct");
        assert_eq!(session.state, "ready");
        assert_eq!(session.profile_id, None);
    }

    let (nav_one, nav_two, nav_three) = tokio::try_join!(
        navigate(&server, &direct_one.session_id, "https://example.com/"),
        navigate(&server, &direct_two.session_id, "https://example.com/"),
        navigate(&server, &direct_three.session_id, "https://example.com/")
    )?;
    for nav in [nav_one, nav_two, nav_three] {
        assert_eq!(nav.ready_state, "complete");
        assert_eq!(nav.url, "https://example.com/");
    }

    let (title_one, title_two, title_three) = tokio::try_join!(
        evaluate_title(&server, &direct_one.session_id),
        evaluate_title(&server, &direct_two.session_id),
        evaluate_title(&server, &direct_three.session_id)
    )?;
    for title in [title_one, title_two, title_three] {
        assert_eq!(title, "Example Domain");
    }

    let shared_profile = create_profile(
        &server,
        "same-profile-readonly",
        "shared profile for concurrent direct read_only sessions",
    )
    .await?;
    let readonly_one =
        create_session(&server, Some(&shared_profile.profile_id), Some("read_only")).await?;
    let readonly_two =
        create_session(&server, Some(&shared_profile.profile_id), Some("read_only")).await?;

    assert_eq!(
        readonly_one.profile_id.as_deref(),
        Some(shared_profile.profile_id.as_str())
    );
    assert_eq!(
        readonly_two.profile_id.as_deref(),
        Some(shared_profile.profile_id.as_str())
    );
    assert_eq!(readonly_one.profile_mode, "read_only");
    assert_eq!(readonly_two.profile_mode, "read_only");
    assert_eq!(readonly_one.proxy_policy, "direct");
    assert_eq!(readonly_two.proxy_policy, "direct");

    let locked_profile = create_profile(
        &server,
        "same-profile-readwrite",
        "profile used to verify read_write locking",
    )
    .await?;
    let readwrite_one = create_session(
        &server,
        Some(&locked_profile.profile_id),
        Some("read_write"),
    )
    .await?;
    assert_eq!(readwrite_one.profile_mode, "read_write");
    assert_eq!(
        readwrite_one.profile_id.as_deref(),
        Some(locked_profile.profile_id.as_str())
    );

    let rejected = server
        .create_session_raw(json!({
            "tenant_id": null,
            "profile_id": locked_profile.profile_id,
            "profile_mode": "read_write",
            "allowed_domains": [],
            "denied_domains": [],
            "proxy_policy": "direct",
        }))
        .await?;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    let status: StatusResponse = server.get_json("/v1/status").await?;
    assert_eq!(status.active_sessions, 6);
    assert_eq!(status.total_sessions, 6);

    for session in [
        &direct_one,
        &direct_two,
        &direct_three,
        &readonly_one,
        &readonly_two,
        &readwrite_one,
    ] {
        let closed = close_session(&server, &session.session_id).await?;
        assert_eq!(closed.session_id, session.session_id);
        assert_eq!(closed.state, "closed");
    }

    let status: StatusResponse = server.get_json("/v1/status").await?;
    assert_eq!(status.active_sessions, 0);
    assert_eq!(status.total_sessions, 6);

    let sessions: Vec<SessionRecord> = server.get_json("/v1/sessions").await?;
    assert_eq!(sessions.len(), 6);
    for session in sessions {
        assert_eq!(session.state, "closed");
        assert_eq!(session.proxy_policy, "direct");
    }

    server.shutdown().await
}
