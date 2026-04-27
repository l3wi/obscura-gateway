use std::sync::Arc;

use anyhow::Result;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use utoipa::{OpenApi, ToSchema};

use crate::config::AppConfig;
use crate::cookies::{
    detect_format_from_name, export_json, export_netscape, parse_cookies, validate_non_empty,
};
use crate::gateway::{Gateway, write_bytes};
use crate::models::{
    ArtifactEntry, CreateProfileRequest, CreateSessionRequest, DumpFormat, DumpLink,
    DumpSessionRequest, DumpSessionResponse, EvaluateSessionRequest, EvaluateSessionResponse,
    GatewayEvent, GrantResponse, MAX_CONCURRENT_SESSIONS, NavigateSessionRequest,
    NavigateSessionResponse, ProfileCookiesImportResponse, ProfileRecord, QuotasResponse,
    ServerStatusResponse, SessionRecord, UpdateProfileRequest,
};

#[derive(Clone)]
pub struct AppState {
    pub gateway: Arc<Gateway>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        create_session,
        list_sessions,
        get_session,
        delete_session,
        mint_grant,
        navigate_session,
        evaluate_session,
        dump_session,
        create_profile,
        list_profiles,
        get_profile,
        update_profile,
        delete_profile,
        status,
        list_artifacts,
        quotas
    ),
    components(
        schemas(
            SessionRecord,
            ProfileRecord,
            ArtifactEntry,
            CreateSessionRequest,
            NavigateSessionRequest,
            NavigateSessionResponse,
            EvaluateSessionRequest,
            EvaluateSessionResponse,
            DumpSessionRequest,
            DumpSessionResponse,
            DumpFormat,
            DumpLink,
            CreateProfileRequest,
            UpdateProfileRequest,
            GrantResponse,
            QuotasResponse,
            ServerStatusResponse,
            ProfileCookiesImportResponse,
            GatewayEvent
        )
    ),
    tags(
        (name = "gateway", description = "Obscura gateway control plane")
    )
)]
pub struct ApiDoc;

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/openapi.json", get(openapi))
        .route("/v1/status", get(status))
        .route("/v1/quotas", get(quotas))
        .route("/v1/sessions", post(create_session).get(list_sessions))
        .route("/v1/sessions/{id}", get(get_session).delete(delete_session))
        .route("/v1/sessions/{id}/grants/cdp", post(mint_grant))
        .route("/v1/sessions/{id}/actions/navigate", post(navigate_session))
        .route("/v1/sessions/{id}/actions/eval", post(evaluate_session))
        .route("/v1/sessions/{id}/actions/dump", post(dump_session))
        .route("/v1/sessions/{id}/artifacts", get(list_artifacts))
        .route("/v1/sessions/{id}/events", get(session_events))
        .route("/v1/profiles", post(create_profile).get(list_profiles))
        .route(
            "/v1/profiles/{id}",
            get(get_profile)
                .patch(update_profile)
                .delete(delete_profile),
        )
        .route("/v1/profiles/{id}/cookies:import", post(import_cookies))
        .route("/v1/profiles/{id}/cookies:export", get(export_cookies))
        .route("/v1/cdp/{id}", get(cdp_proxy))
        .with_state(state)
        .layer(DefaultBodyLimit::max(20 * 1024 * 1024))
}

fn require_auth(headers: &HeaderMap, config: &AppConfig) -> Result<(), StatusCode> {
    let expected = format!("Bearer {}", config.api_key);
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if auth == expected {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true }))
}

async fn openapi() -> impl IntoResponse {
    Json(ApiDoc::openapi())
}

#[utoipa::path(get, path = "/v1/status", responses((status = 200, body = ServerStatusResponse)), tag = "gateway")]
async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ServerStatusResponse>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    let obscura_bin = config.obscura_bin.display().to_string();
    let saved_profiles = state
        .gateway
        .db
        .profiles_count()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total_sessions = state
        .gateway
        .db
        .total_sessions_count()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let active_sessions = state
        .gateway
        .db
        .active_sessions_count()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ServerStatusResponse {
        listen_addr: config.listen_addr,
        obscura_bin,
        default_stealth: config.default_stealth,
        default_proxy_policy: config.default_proxy_policy,
        proxy_policies: config.proxy_policies.len(),
        saved_profiles,
        total_sessions,
        active_sessions,
    }))
}

#[utoipa::path(post, path = "/v1/sessions", request_body = CreateSessionRequest, responses((status = 200, body = SessionRecord)), tag = "gateway")]
async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<Json<SessionRecord>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .create_session(payload)
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

#[utoipa::path(get, path = "/v1/sessions", responses((status = 200, body = [SessionRecord])), tag = "gateway")]
async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<SessionRecord>>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .db
        .list_sessions()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[utoipa::path(get, path = "/v1/sessions/{id}", responses((status = 200, body = SessionRecord)), tag = "gateway")]
async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SessionRecord>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .db
        .get_session(&id)
        .map(Json)
        .map_err(|_| StatusCode::NOT_FOUND)
}

#[utoipa::path(delete, path = "/v1/sessions/{id}", responses((status = 200, body = SessionRecord)), tag = "gateway")]
async fn delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SessionRecord>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .close_session(&id, "closed by api")
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

#[utoipa::path(post, path = "/v1/sessions/{id}/grants/cdp", responses((status = 200, body = GrantResponse)), tag = "gateway")]
async fn mint_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<GrantResponse>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .mint_grant(&id, &config.server_url)
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

#[utoipa::path(post, path = "/v1/sessions/{id}/actions/navigate", request_body = NavigateSessionRequest, responses((status = 200, body = NavigateSessionResponse)), tag = "gateway")]
async fn navigate_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<NavigateSessionRequest>,
) -> Result<Json<NavigateSessionResponse>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .navigate_session(&id, payload)
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

#[utoipa::path(post, path = "/v1/sessions/{id}/actions/eval", request_body = EvaluateSessionRequest, responses((status = 200, body = EvaluateSessionResponse)), tag = "gateway")]
async fn evaluate_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<EvaluateSessionRequest>,
) -> Result<Json<EvaluateSessionResponse>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .evaluate_session(&id, payload)
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

#[utoipa::path(post, path = "/v1/sessions/{id}/actions/dump", request_body = DumpSessionRequest, responses((status = 200, body = DumpSessionResponse)), tag = "gateway")]
async fn dump_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<DumpSessionRequest>,
) -> Result<Json<DumpSessionResponse>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .dump_session(&id, payload.format)
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

#[utoipa::path(get, path = "/v1/sessions/{id}/artifacts", responses((status = 200, body = [ArtifactEntry])), tag = "gateway")]
async fn list_artifacts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Vec<ArtifactEntry>>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .list_artifacts(&id)
        .map(Json)
        .map_err(|_| StatusCode::NOT_FOUND)
}

async fn session_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<
    Sse<
        impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
    >,
    StatusCode,
> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    let stream =
        BroadcastStream::new(state.gateway.events_tx.subscribe()).filter_map(move |item| {
            let id = id.clone();
            async move {
                match item {
                    Ok(event) if event.session_id == id => {
                        Some(Ok(axum::response::sse::Event::default()
                            .json_data(event)
                            .ok()?))
                    }
                    _ => None,
                }
            }
        });
    Ok(Sse::new(stream))
}

#[utoipa::path(post, path = "/v1/profiles", request_body = CreateProfileRequest, responses((status = 200, body = ProfileRecord)), tag = "gateway")]
async fn create_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateProfileRequest>,
) -> Result<Json<ProfileRecord>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .create_profile(payload)
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

#[utoipa::path(get, path = "/v1/profiles", responses((status = 200, body = [ProfileRecord])), tag = "gateway")]
async fn list_profiles(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ProfileRecord>>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .db
        .list_profiles()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[utoipa::path(get, path = "/v1/profiles/{id}", responses((status = 200, body = ProfileRecord)), tag = "gateway")]
async fn get_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ProfileRecord>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .db
        .get_profile(&id)
        .map(Json)
        .map_err(|_| StatusCode::NOT_FOUND)
}

#[utoipa::path(patch, path = "/v1/profiles/{id}", request_body = UpdateProfileRequest, responses((status = 200, body = ProfileRecord)), tag = "gateway")]
async fn update_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<UpdateProfileRequest>,
) -> Result<Json<ProfileRecord>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .update_profile(&id, &payload.description, payload.identity)
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

#[utoipa::path(delete, path = "/v1/profiles/{id}", responses((status = 204)), tag = "gateway")]
async fn delete_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    state
        .gateway
        .delete_profile(&id)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

async fn import_cookies(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<ProfileCookiesImportResponse>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    let mut file_name = None;
    let mut bytes = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    {
        if field.name() == Some("file") {
            file_name = field.file_name().map(|v| v.to_string());
            bytes = Some(field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?);
        }
    }
    let bytes = bytes.ok_or(StatusCode::BAD_REQUEST)?;
    let raw = String::from_utf8(bytes.to_vec()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let format = detect_format_from_name(file_name.as_deref());
    let cookies = parse_cookies(&raw, format).map_err(|_| StatusCode::BAD_REQUEST)?;
    validate_non_empty(&cookies).map_err(|_| StatusCode::BAD_REQUEST)?;
    let response = state
        .gateway
        .import_profile_cookies(&id, &cookies)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let artifact_path = state
        .gateway
        .paths
        .profile_dir(&id)
        .join("last-cookie-import");
    write_bytes(&artifact_path, raw.as_bytes()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(response))
}

#[derive(serde::Deserialize, ToSchema)]
struct ExportQuery {
    format: Option<String>,
}

async fn export_cookies(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ExportQuery>,
) -> Result<Response, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    let cookies = state
        .gateway
        .export_profile_cookies(&id)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let format = query.format.unwrap_or_else(|| "json".to_string());
    if format == "netscape" {
        Ok((StatusCode::OK, export_netscape(&cookies)).into_response())
    } else {
        Ok((
            StatusCode::OK,
            export_json(&cookies).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        )
            .into_response())
    }
}

#[utoipa::path(get, path = "/v1/quotas", responses((status = 200, body = QuotasResponse)), tag = "gateway")]
async fn quotas(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<QuotasResponse>, StatusCode> {
    let config = state.gateway.config.read().await.clone();
    require_auth(&headers, &config)?;
    Ok(Json(QuotasResponse {
        max_concurrent_sessions: MAX_CONCURRENT_SESSIONS,
        active_sessions: state
            .gateway
            .db
            .active_sessions_count()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        profiles: state
            .gateway
            .db
            .profiles_count()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    }))
}

#[derive(serde::Deserialize)]
struct GrantQuery {
    grant: String,
}

async fn cdp_proxy(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<GrantQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        let _ = state.gateway.proxy_cdp(&id, &query.grant, socket).await;
    })
}
