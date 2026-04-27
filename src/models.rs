use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub const MAX_CONCURRENT_SESSIONS: usize = 25;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfiguredRole {
    Server,
    Cli,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusSource {
    Local,
    Remote,
    ConfigOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionRecord {
    pub session_id: String,
    pub tenant_id: Option<String>,
    pub profile_id: Option<String>,
    pub profile_mode: ProfileMode,
    pub state: SessionState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub idle_deadline: DateTime<Utc>,
    pub absolute_deadline: DateTime<Utc>,
    pub cdp_ws_url: Option<String>,
    pub child_pid: Option<u32>,
    pub stealth: bool,
    pub proxy_policy: String,
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
    pub close_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Provisioning,
    Ready,
    Attached,
    Idle,
    Closing,
    Closed,
    Expired,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProfileRecord {
    pub profile_id: String,
    pub name: String,
    pub description: String,
    pub identity: ProfileIdentity,
    pub cookie_urls: Vec<String>,
    pub cookie_count: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Default)]
pub struct ProfileIdentity {
    pub stealth: Option<bool>,
    pub user_agent: Option<String>,
    pub accept_language: Option<String>,
    pub timezone: Option<String>,
    pub viewport: Option<ViewportConfig>,
    pub proxy_affinity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ViewportConfig {
    pub width: u32,
    pub height: u32,
    pub screen_width: Option<u32>,
    pub screen_height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateSessionRequest {
    pub tenant_id: Option<String>,
    pub profile_id: Option<String>,
    pub profile_mode: Option<ProfileMode>,
    pub stealth: Option<bool>,
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub denied_domains: Vec<String>,
    pub proxy_policy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NavigateSessionRequest {
    pub url: String,
    #[serde(default = "default_wait_until")]
    pub wait_until: String,
    pub timeout_secs: Option<u64>,
}

fn default_wait_until() -> String {
    "load".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvaluateSessionRequest {
    pub expression: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DumpSessionRequest {
    pub format: DumpFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DumpFormat {
    Html,
    Text,
    Links,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NavigateSessionResponse {
    pub session_id: String,
    pub target_id: String,
    pub url: String,
    pub ready_state: String,
    pub loader_id: Option<String>,
    pub frame_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvaluateSessionResponse {
    pub session_id: String,
    pub target_id: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DumpLink {
    pub url: String,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum DumpSessionResponse {
    Html {
        session_id: String,
        target_id: String,
        content: String,
    },
    Text {
        session_id: String,
        target_id: String,
        content: String,
    },
    Links {
        session_id: String,
        target_id: String,
        links: Vec<DumpLink>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateProfileRequest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub identity: ProfileIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateProfileRequest {
    pub description: String,
    pub identity: Option<ProfileIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GrantResponse {
    pub grant_id: String,
    pub ws_url: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QuotasResponse {
    pub max_concurrent_sessions: usize,
    pub active_sessions: usize,
    pub profiles: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServerStatusResponse {
    pub listen_addr: String,
    pub obscura_bin: String,
    pub default_stealth: bool,
    pub default_proxy_policy: String,
    pub proxy_policies: usize,
    pub saved_profiles: usize,
    pub total_sessions: usize,
    pub active_sessions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CliStatusResponse {
    pub configured_role: ConfiguredRole,
    pub status_source: StatusSource,
    pub config_root: String,
    pub server_url: String,
    pub listen_addr: String,
    pub api_key_configured: bool,
    pub server_reachable: bool,
    pub server: Option<ServerStatusResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ArtifactEntry {
    pub name: String,
    pub relative_path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProfileCookiesImportResponse {
    pub profile_id: String,
    pub imported: usize,
    pub cookie_urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GatewayEvent {
    pub event_id: String,
    pub session_id: String,
    pub kind: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CdpGrantRecord {
    pub grant_id: String,
    pub session_id: String,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRuntime {
    pub session_id: String,
    pub child_pid: u32,
    pub cdp_port: u16,
    pub local_ws_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub expires: Option<i64>,
}
