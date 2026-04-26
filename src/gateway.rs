use std::collections::HashMap;
use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::ws::{Message as AxumWsMessage, WebSocket};
use base64::Engine;
use chrono::{Duration, Utc};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock, broadcast};
use tokio::time::{Duration as TokioDuration, Instant, sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message as TungsteniteMessage};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::config::{AppConfig, AppPaths};
use crate::cookies::{cookie_urls, export_json, export_netscape};
use crate::db::Database;
use crate::models::{
    ArtifactEntry, CdpGrantRecord, CreateProfileRequest, CreateSessionRequest, DumpFormat,
    DumpLink, DumpSessionResponse, EvaluateSessionRequest, EvaluateSessionResponse, GatewayEvent,
    GrantResponse, NavigateSessionRequest, NavigateSessionResponse, ProfileCookiesImportResponse,
    ProfileIdentity, ProfileMode, ProfileRecord, SessionRecord, SessionRuntime, SessionState,
    StoredCookie,
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct Gateway {
    pub paths: AppPaths,
    pub config: Arc<RwLock<AppConfig>>,
    pub db: Database,
    runtimes: Arc<Mutex<HashMap<String, ManagedSession>>>,
    pub events_tx: broadcast::Sender<GatewayEvent>,
}

struct ManagedSession {
    runtime: SessionRuntime,
    child: Child,
    target_id: Option<String>,
}

impl Gateway {
    pub fn new(paths: AppPaths, config: AppConfig, db: Database) -> Self {
        let (events_tx, _) = broadcast::channel(512);
        Self {
            paths,
            config: Arc::new(RwLock::new(config)),
            db,
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            events_tx,
        }
    }

    pub async fn create_profile(&self, request: CreateProfileRequest) -> Result<ProfileRecord> {
        let now = Utc::now();
        let profile = ProfileRecord {
            profile_id: Uuid::new_v4().to_string(),
            name: request.name,
            description: request.description,
            identity: request.identity,
            cookie_urls: Vec::new(),
            cookie_count: 0,
            created_at: now,
            updated_at: now,
            last_used_at: None,
        };
        fs::create_dir_all(self.paths.profile_dir(&profile.profile_id))?;
        self.db.insert_profile(&profile)?;
        Ok(profile)
    }

    pub async fn update_profile(
        &self,
        profile_id: &str,
        description: &str,
        identity: Option<ProfileIdentity>,
    ) -> Result<ProfileRecord> {
        let profile = self.db.get_profile(profile_id)?;
        let identity = identity.unwrap_or(profile.identity.clone());
        self.db.update_profile_metadata(
            profile_id,
            description,
            &identity,
            &profile.cookie_urls,
            profile.cookie_count,
            profile.last_used_at,
        )?;
        self.db.get_profile(profile_id)
    }

    pub async fn import_profile_cookies(
        &self,
        profile_id: &str,
        cookies: &[StoredCookie],
    ) -> Result<ProfileCookiesImportResponse> {
        let profile = self.db.get_profile(profile_id)?;
        if profile.description.trim().is_empty() {
            bail!("profile description is required before importing cookies");
        }
        let active_sessions = self.db.active_sessions_for_profile(profile_id)?;
        if !active_sessions.is_empty() {
            bail!(
                "cannot import cookies while {} active session(s) are attached to this profile",
                active_sessions.len()
            );
        }
        fs::create_dir_all(self.paths.profile_dir(profile_id))?;
        fs::write(
            self.paths.profile_json_cookie_path(profile_id),
            export_json(cookies)?,
        )?;
        fs::write(
            self.paths.profile_netscape_cookie_path(profile_id),
            export_netscape(cookies),
        )?;
        let urls = cookie_urls(cookies);
        self.db.update_profile_metadata(
            profile_id,
            &profile.description,
            &profile.identity,
            &urls,
            cookies.len(),
            Some(Utc::now()),
        )?;
        Ok(ProfileCookiesImportResponse {
            profile_id: profile_id.to_string(),
            imported: cookies.len(),
            cookie_urls: urls,
        })
    }

    pub async fn export_profile_cookies(&self, profile_id: &str) -> Result<Vec<StoredCookie>> {
        let _ = self.db.get_profile(profile_id)?;
        let path = self.paths.profile_json_cookie_path(profile_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub async fn create_session(&self, request: CreateSessionRequest) -> Result<SessionRecord> {
        let config = self.config.read().await.clone();
        if !request.allowed_domains.is_empty() && !request.denied_domains.is_empty() {
            for denied in &request.denied_domains {
                if request.allowed_domains.contains(denied) {
                    bail!("domain cannot exist in both allow and deny lists");
                }
            }
        }

        let profile_mode = request.profile_mode.unwrap_or(ProfileMode::ReadOnly);
        let mut profile_identity = None;
        if let Some(profile_id) = &request.profile_id {
            let profile = self.db.get_profile(profile_id)?;
            profile_identity = Some(profile.identity.clone());
            if profile_mode == ProfileMode::ReadWrite {
                let active = self.db.active_sessions_for_profile(profile_id)?;
                let conflicting = active
                    .iter()
                    .any(|session| session.profile_mode == ProfileMode::ReadWrite);
                if conflicting {
                    bail!("profile already has an active read_write session");
                }
            }
        }

        let session_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let listener_port = pick_free_port()?;
        let artifact_dir = self.paths.session_artifact_dir(&session_id);
        fs::create_dir_all(&artifact_dir)?;

        let resolved_proxy_policy = request
            .proxy_policy
            .or_else(|| {
                profile_identity
                    .as_ref()
                    .and_then(|i| i.proxy_affinity.clone())
            })
            .unwrap_or_else(|| config.default_proxy_policy.clone());
        let resolved_proxy_url = config.resolve_proxy_url(&resolved_proxy_policy)?;

        let local_ws_url = format!("ws://127.0.0.1:{listener_port}/devtools/browser");
        let mut command = Command::new(&config.obscura_bin);
        command
            .arg("serve")
            .arg("--port")
            .arg(listener_port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(proxy_url) = &resolved_proxy_url {
            command.arg("--proxy").arg(proxy_url);
        }
        let child = command.spawn().with_context(|| {
            format!(
                "failed to launch obscura child at {}",
                config.obscura_bin.display()
            )
        })?;
        let child_pid = child.id().unwrap_or_default();
        let runtime = SessionRuntime {
            session_id: session_id.clone(),
            child_pid,
            cdp_port: listener_port,
            local_ws_url: local_ws_url.clone(),
        };

        let record = SessionRecord {
            session_id: session_id.clone(),
            tenant_id: request.tenant_id,
            profile_id: request.profile_id.clone(),
            profile_mode: profile_mode.clone(),
            state: SessionState::Ready,
            created_at: now,
            updated_at: now,
            idle_deadline: now + Duration::seconds(config.idle_ttl_secs),
            absolute_deadline: now + Duration::seconds(config.absolute_ttl_secs),
            cdp_ws_url: Some(local_ws_url.clone()),
            child_pid: Some(child_pid),
            proxy_policy: resolved_proxy_policy,
            allowed_domains: request.allowed_domains,
            denied_domains: request.denied_domains,
            close_reason: None,
        };
        self.db.insert_session(&record)?;
        self.runtimes.lock().await.insert(
            session_id.clone(),
            ManagedSession {
                runtime: runtime.clone(),
                child,
                target_id: None,
            },
        );
        self.emit_event(&session_id, "session.created", "session launched")?;

        if let Some(profile_id) = &request.profile_id {
            let cookies = self.export_profile_cookies(profile_id).await?;
            if !cookies.is_empty() {
                self.inject_cookies(&runtime.local_ws_url, &cookies).await?;
                self.db.touch_profile_last_used(profile_id)?;
                self.emit_event(
                    &session_id,
                    "profile.attached",
                    &format!("profile cookies injected ({profile_mode:?})"),
                )?;
            }
        }
        Ok(self.db.get_session(&session_id)?)
    }

    pub async fn close_session(&self, session_id: &str, reason: &str) -> Result<SessionRecord> {
        if let Some(managed) = self.runtimes.lock().await.remove(session_id) {
            let session = self.db.get_session(session_id)?;
            if let Some(profile_id) = session
                .profile_id
                .clone()
                .filter(|_| session.profile_mode == ProfileMode::ReadWrite)
            {
                let cookies = self
                    .fetch_cookies(&managed.runtime.local_ws_url)
                    .await
                    .unwrap_or_default();
                if !cookies.is_empty() {
                    let profile = self.db.get_profile(&profile_id)?;
                    self.db.update_profile_metadata(
                        &profile_id,
                        &profile.description,
                        &profile.identity,
                        &cookie_urls(&cookies),
                        cookies.len(),
                        Some(Utc::now()),
                    )?;
                    fs::write(
                        self.paths.profile_json_cookie_path(&profile_id),
                        export_json(&cookies)?,
                    )?;
                    fs::write(
                        self.paths.profile_netscape_cookie_path(&profile_id),
                        export_netscape(&cookies),
                    )?;
                }
            }
            let mut child = managed.child;
            let _ = child.kill().await;
        }
        self.db
            .update_session_state(session_id, SessionState::Closed, None, None, Some(reason))?;
        self.emit_event(session_id, "session.closed", reason)?;
        Ok(self.db.get_session(session_id)?)
    }

    pub async fn mint_grant(&self, session_id: &str, public_base: &str) -> Result<GrantResponse> {
        let config = self.config.read().await.clone();
        let _ = self.db.get_session(session_id)?;
        let grant_id = Uuid::new_v4().to_string();
        let expires_at = Utc::now() + Duration::seconds(config.connect_ttl_secs);
        let payload = format!("{grant_id}:{session_id}:{}", expires_at.timestamp());
        let token = sign_token(&config.api_key, &payload)?;
        self.db.insert_grant(&CdpGrantRecord {
            grant_id: grant_id.clone(),
            session_id: session_id.to_string(),
            token: token.clone(),
            expires_at,
            used_at: None,
        })?;
        Ok(GrantResponse {
            grant_id,
            ws_url: format!(
                "{}/v1/cdp/{session_id}?grant={token}",
                public_base.trim_end_matches('/')
            ),
            expires_at,
        })
    }

    pub async fn navigate_session(
        &self,
        session_id: &str,
        request: NavigateSessionRequest,
    ) -> Result<NavigateSessionResponse> {
        let runtime = self.get_runtime(session_id).await?;
        let target_id = self.ensure_target(session_id).await?;
        let identity = self.profile_identity_for_session(session_id)?;
        let mut cdp = self
            .attach_to_target(&runtime.local_ws_url, &target_id, identity.as_ref())
            .await?;
        let nav = cdp
            .request(
                "Page.navigate",
                Some(serde_json::json!({ "url": request.url })),
            )
            .await?;
        let timeout = request.timeout_secs.unwrap_or(20);
        let ready_state = wait_for_ready_state(&mut cdp, &request.wait_until, timeout).await?;
        let url = cdp.eval_string("location.href").await?;
        self.db.update_session_state(
            session_id,
            SessionState::Idle,
            Some(runtime.child_pid),
            None,
            None,
        )?;
        self.emit_event(session_id, "session.navigate", &url)?;
        Ok(NavigateSessionResponse {
            session_id: session_id.to_string(),
            target_id,
            url,
            ready_state,
            loader_id: nav
                .get("loaderId")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            frame_id: nav
                .get("frameId")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
        })
    }

    pub async fn evaluate_session(
        &self,
        session_id: &str,
        request: EvaluateSessionRequest,
    ) -> Result<EvaluateSessionResponse> {
        let runtime = self.get_runtime(session_id).await?;
        let target_id = self.ensure_target(session_id).await?;
        let identity = self.profile_identity_for_session(session_id)?;
        let mut cdp = self
            .attach_to_target(&runtime.local_ws_url, &target_id, identity.as_ref())
            .await?;
        let value = cdp.evaluate(&request.expression).await?;
        self.db.update_session_state(
            session_id,
            SessionState::Idle,
            Some(runtime.child_pid),
            None,
            None,
        )?;
        self.emit_event(session_id, "session.eval", &request.expression)?;
        Ok(EvaluateSessionResponse {
            session_id: session_id.to_string(),
            target_id,
            value,
        })
    }

    pub async fn dump_session(
        &self,
        session_id: &str,
        format: DumpFormat,
    ) -> Result<DumpSessionResponse> {
        let runtime = self.get_runtime(session_id).await?;
        let target_id = self.ensure_target(session_id).await?;
        let identity = self.profile_identity_for_session(session_id)?;
        let mut cdp = self
            .attach_to_target(&runtime.local_ws_url, &target_id, identity.as_ref())
            .await?;
        let response = match format {
            DumpFormat::Html => DumpSessionResponse::Html {
                session_id: session_id.to_string(),
                target_id: target_id.clone(),
                content: cdp
                    .eval_string(
                        "document.documentElement ? document.documentElement.outerHTML : ''",
                    )
                    .await?,
            },
            DumpFormat::Text => DumpSessionResponse::Text {
                session_id: session_id.to_string(),
                target_id: target_id.clone(),
                content: cdp
                    .eval_string("document.body ? document.body.innerText : ''")
                    .await?,
            },
            DumpFormat::Links => {
                let value = cdp
                    .evaluate(
                        "Array.from(document.querySelectorAll('a')).map(a => ({ url: a.href, text: (a.innerText || a.textContent || '').trim() || null })).filter(v => v.url)",
                    )
                    .await?;
                let links: Vec<DumpLink> = serde_json::from_value(value)?;
                DumpSessionResponse::Links {
                    session_id: session_id.to_string(),
                    target_id: target_id.clone(),
                    links,
                }
            }
        };
        self.db.update_session_state(
            session_id,
            SessionState::Idle,
            Some(runtime.child_pid),
            None,
            None,
        )?;
        self.emit_event(session_id, "session.dump", &format!("{format:?}"))?;
        Ok(response)
    }

    pub async fn proxy_cdp(&self, token: &str, socket: WebSocket) -> Result<()> {
        let grant = self.db.use_grant(token)?;
        let session = self.db.get_session(&grant.session_id)?;
        let target = session
            .cdp_ws_url
            .clone()
            .ok_or_else(|| anyhow!("session missing local websocket"))?;
        self.db.update_session_state(
            &grant.session_id,
            SessionState::Attached,
            session.child_pid,
            Some(&target),
            None,
        )?;
        self.emit_event(&grant.session_id, "cdp.attached", "grant consumed")?;

        let (upstream, _) = connect_async(target).await?;
        let (mut upstream_tx, mut upstream_rx) = upstream.split();
        let (mut downstream_tx, mut downstream_rx) = socket.split();

        let forward_up = async {
            while let Some(message) = downstream_rx.next().await {
                match message? {
                    AxumWsMessage::Text(text) => {
                        upstream_tx
                            .send(TungsteniteMessage::Text(text.to_string().into()))
                            .await?
                    }
                    AxumWsMessage::Binary(data) => {
                        upstream_tx.send(TungsteniteMessage::Binary(data)).await?
                    }
                    AxumWsMessage::Close(frame) => {
                        upstream_tx
                            .send(TungsteniteMessage::Close(frame.map(|f| {
                                tokio_tungstenite::tungstenite::protocol::CloseFrame {
                                    code: f.code.into(),
                                    reason: f.reason.to_string().into(),
                                }
                            })))
                            .await?;
                        break;
                    }
                    AxumWsMessage::Ping(data) => {
                        upstream_tx.send(TungsteniteMessage::Ping(data)).await?
                    }
                    AxumWsMessage::Pong(data) => {
                        upstream_tx.send(TungsteniteMessage::Pong(data)).await?
                    }
                }
            }
            Result::<()>::Ok(())
        };

        let forward_down = async {
            while let Some(message) = upstream_rx.next().await {
                match message? {
                    TungsteniteMessage::Text(text) => {
                        downstream_tx
                            .send(AxumWsMessage::Text(text.to_string().into()))
                            .await?
                    }
                    TungsteniteMessage::Binary(data) => {
                        downstream_tx.send(AxumWsMessage::Binary(data)).await?
                    }
                    TungsteniteMessage::Close(frame) => {
                        let close = frame.map(|f| axum::extract::ws::CloseFrame {
                            code: f.code.into(),
                            reason: f.reason.to_string().into(),
                        });
                        downstream_tx.send(AxumWsMessage::Close(close)).await?;
                        break;
                    }
                    TungsteniteMessage::Ping(data) => {
                        downstream_tx.send(AxumWsMessage::Ping(data)).await?
                    }
                    TungsteniteMessage::Pong(data) => {
                        downstream_tx.send(AxumWsMessage::Pong(data)).await?
                    }
                    TungsteniteMessage::Frame(_) => {}
                }
            }
            Result::<()>::Ok(())
        };

        tokio::try_join!(forward_up, forward_down)?;
        self.db.update_session_state(
            &grant.session_id,
            SessionState::Idle,
            session.child_pid,
            None,
            None,
        )?;
        Ok(())
    }

    pub fn list_artifacts(&self, session_id: &str) -> Result<Vec<ArtifactEntry>> {
        let root = self.paths.session_artifact_dir(session_id);
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        for entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
            if entry.file_type().is_file() {
                let path = entry.into_path();
                let metadata = fs::metadata(&path)?;
                let relative = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                entries.push(ArtifactEntry {
                    name: relative.clone(),
                    relative_path: relative,
                    size_bytes: metadata.len(),
                });
            }
        }
        Ok(entries)
    }

    pub fn emit_event(&self, session_id: &str, kind: &str, message: &str) -> Result<()> {
        let event = GatewayEvent {
            event_id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            kind: kind.to_string(),
            message: message.to_string(),
            created_at: Utc::now(),
        };
        self.db.insert_event(&event)?;
        let _ = self.events_tx.send(event);
        Ok(())
    }

    async fn inject_cookies(&self, ws_url: &str, cookies: &[StoredCookie]) -> Result<()> {
        if cookies.is_empty() {
            return Ok(());
        }
        let (mut stream, _) = connect_async(ws_url).await?;
        let cookies_json = serde_json::to_value(cookies)?;
        let request = serde_json::json!({
            "id": 1,
            "method": "Storage.setCookies",
            "params": { "cookies": cookies_json }
        });
        stream
            .send(TungsteniteMessage::Text(request.to_string().into()))
            .await?;
        let _ = stream.next().await;
        Ok(())
    }

    async fn fetch_cookies(&self, ws_url: &str) -> Result<Vec<StoredCookie>> {
        let (mut stream, _) = connect_async(ws_url).await?;
        let request = serde_json::json!({
            "id": 1,
            "method": "Storage.getCookies",
            "params": {}
        });
        stream
            .send(TungsteniteMessage::Text(request.to_string().into()))
            .await?;
        while let Some(message) = stream.next().await {
            let message = message?;
            if let TungsteniteMessage::Text(text) = message {
                let value: serde_json::Value = serde_json::from_str(&text)?;
                if let Some(cookies) = value.get("result").and_then(|r| r.get("cookies")).cloned() {
                    let parsed: Vec<StoredCookie> = serde_json::from_value(cookies)?;
                    return Ok(parsed);
                }
            }
        }
        Ok(Vec::new())
    }

    async fn apply_profile_identity_to_connection(
        &self,
        cdp: &mut CdpConnection,
        identity: &ProfileIdentity,
    ) -> Result<()> {
        if identity.user_agent.is_none()
            && identity.accept_language.is_none()
            && identity.timezone.is_none()
            && identity.viewport.is_none()
        {
            return Ok(());
        }

        if identity.user_agent.is_some() || identity.accept_language.is_some() {
            let mut params = serde_json::Map::new();
            if let Some(user_agent) = &identity.user_agent {
                params.insert("userAgent".to_string(), Value::String(user_agent.clone()));
            }
            if let Some(accept_language) = &identity.accept_language {
                params.insert(
                    "acceptLanguage".to_string(),
                    Value::String(accept_language.clone()),
                );
            }
            let _ = cdp
                .request("Network.setUserAgentOverride", Some(Value::Object(params)))
                .await;
        }

        if let Some(accept_language) = &identity.accept_language {
            let _ = cdp
                .request(
                    "Network.setExtraHTTPHeaders",
                    Some(serde_json::json!({
                        "headers": {
                            "Accept-Language": accept_language
                        }
                    })),
                )
                .await;
        }

        // Obscura does not currently expose stable CDP emulation primitives for timezone or
        // viewport/screen overrides, so these fields are persisted but not enforced here.
        Ok(())
    }

    async fn create_temporary_target(&self, ws_url: &str) -> Result<String> {
        let (mut stream, _) = connect_async(ws_url).await?;
        let request = serde_json::json!({
            "id": 1,
            "method": "Target.createTarget",
            "params": { "url": "about:blank" }
        });
        stream
            .send(TungsteniteMessage::Text(request.to_string().into()))
            .await?;
        while let Some(message) = stream.next().await {
            let message = message?;
            if let TungsteniteMessage::Text(text) = message {
                let value: Value = serde_json::from_str(&text)?;
                if value.get("id").and_then(|v| v.as_i64()) == Some(1) {
                    return value
                        .get("result")
                        .and_then(|r| r.get("targetId"))
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string)
                        .ok_or_else(|| anyhow!("failed to create target"));
                }
            }
        }
        bail!("failed to create target")
    }

    async fn get_runtime(&self, session_id: &str) -> Result<SessionRuntime> {
        let runtimes = self.runtimes.lock().await;
        let managed = runtimes
            .get(session_id)
            .ok_or_else(|| anyhow!("session is not active"))?;
        Ok(managed.runtime.clone())
    }

    async fn ensure_target(&self, session_id: &str) -> Result<String> {
        {
            let runtimes = self.runtimes.lock().await;
            if let Some(target_id) = runtimes
                .get(session_id)
                .and_then(|managed| managed.target_id.clone())
            {
                return Ok(target_id);
            }
        }

        let runtime = self.get_runtime(session_id).await?;
        let target_id = self.create_temporary_target(&runtime.local_ws_url).await?;
        let mut runtimes = self.runtimes.lock().await;
        if let Some(managed) = runtimes.get_mut(session_id) {
            managed.target_id = Some(target_id.clone());
        }
        Ok(target_id)
    }

    async fn attach_to_target(
        &self,
        ws_url: &str,
        target_id: &str,
        identity: Option<&ProfileIdentity>,
    ) -> Result<CdpConnection> {
        let (stream, _) = connect_async(ws_url).await?;
        let mut cdp = CdpConnection {
            stream,
            next_id: 1,
            session_id: None,
        };
        let result = cdp
            .request(
                "Target.attachToTarget",
                Some(serde_json::json!({
                    "targetId": target_id,
                    "flatten": true
                })),
            )
            .await?;
        let session_id = result
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("attach did not return sessionId"))?
            .to_string();
        cdp.session_id = Some(session_id);
        let _ = cdp.request("Page.enable", None).await?;
        let _ = cdp.request("Runtime.enable", None).await?;
        if let Some(identity) = identity {
            self.apply_profile_identity_to_connection(&mut cdp, identity)
                .await?;
        }
        Ok(cdp)
    }

    fn profile_identity_for_session(&self, session_id: &str) -> Result<Option<ProfileIdentity>> {
        let session = self.db.get_session(session_id)?;
        match session.profile_id {
            Some(profile_id) => Ok(Some(self.db.get_profile(&profile_id)?.identity)),
            None => Ok(None),
        }
    }
}

pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn sign_token(secret: &str, payload: &str) -> Result<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())?;
    mac.update(payload.as_bytes());
    let signature =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!(
        "{}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload),
        signature
    ))
}

struct CdpConnection {
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: i64,
    session_id: Option<String>,
}

impl CdpConnection {
    async fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut payload = serde_json::json!({
            "id": id,
            "method": method,
        });
        if let Some(params) = params {
            payload["params"] = params;
        }
        if let Some(session_id) = &self.session_id {
            payload["sessionId"] = Value::String(session_id.clone());
        }
        self.stream
            .send(TungsteniteMessage::Text(payload.to_string().into()))
            .await?;
        loop {
            let message = self
                .stream
                .next()
                .await
                .ok_or_else(|| anyhow!("cdp connection closed"))??;
            if let TungsteniteMessage::Text(text) = message {
                let value: Value = serde_json::from_str(&text)?;
                if value.get("id").and_then(|v| v.as_i64()) == Some(id) {
                    if let Some(error) = value.get("error") {
                        bail!("cdp error for {method}: {}", error);
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
            }
        }
    }

    async fn evaluate(&mut self, expression: &str) -> Result<Value> {
        let result = self
            .request(
                "Runtime.evaluate",
                Some(serde_json::json!({
                    "expression": expression,
                    "returnByValue": true
                })),
            )
            .await?;
        Ok(result
            .get("result")
            .and_then(|v| v.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    async fn eval_string(&mut self, expression: &str) -> Result<String> {
        let value = self.evaluate(expression).await?;
        Ok(value.as_str().unwrap_or_default().to_string())
    }
}

async fn wait_for_ready_state(
    cdp: &mut CdpConnection,
    wait_until: &str,
    timeout_secs: u64,
) -> Result<String> {
    let deadline = Instant::now() + TokioDuration::from_secs(timeout_secs.max(1));
    loop {
        let ready_state = cdp.eval_string("document.readyState").await?;
        let done = match wait_until {
            "domcontentloaded" => matches!(ready_state.as_str(), "interactive" | "complete"),
            "networkidle" => ready_state == "complete",
            _ => ready_state == "complete",
        };
        if done {
            if wait_until == "networkidle" {
                sleep(TokioDuration::from_millis(750)).await;
            }
            return Ok(ready_state);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for {wait_until}");
        }
        sleep(TokioDuration::from_millis(200)).await;
    }
}
