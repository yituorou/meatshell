//! Authenticated Streamable HTTP adapter. The official MCP SDK owns protocol
//! parsing/session transport; the boundary here owns authentication and limits.
use super::oauth::{self, OAuth, OAuthConfig, Principal};
use anyhow::{ensure, Context, Result};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures::StreamExt;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        session::{local::LocalSessionManager, SessionManager},
        StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData, RoleServer, ServerHandler,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, net::SocketAddr, path::Path, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

const MAX_BODY: usize = 1024 * 1024;
const SESSION_TTL: u64 = 15 * 60;
const MAX_SESSIONS: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpConfig {
    #[serde(default = "default_bind")]
    bind: SocketAddr,
    oauth: OAuthConfig,
    #[serde(default)]
    allowed_origins: Vec<String>,
}
fn default_bind() -> SocketAddr {
    "127.0.0.1:8765".parse().unwrap()
}

struct SessionOwner {
    subject: String,
    expires_at: u64,
    cancellation: CancellationToken,
}
impl Drop for SessionOwner {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}
#[derive(Clone)]
struct SessionCancellation(CancellationToken);
struct Boundary {
    oauth: OAuth,
    origins: Vec<String>,
    owners: Mutex<HashMap<String, SessionOwner>>,
    initialize_lock: Mutex<()>,
    sessions: Arc<LocalSessionManager>,
    requests: Semaphore,
    streams: Arc<Semaphore>,
}

#[derive(Clone)]
struct HttpTools {
    allow_config_import: bool,
    scope: String,
    calls: Arc<Semaphore>,
}

// Preserve provider-specific descriptor fields that rmcp 2.2's typed Tool
// intentionally does not model, without modifying transport/SSE serialization.
#[derive(Clone)]
struct HttpService(HttpTools);
impl rmcp::Service<RoleServer> for HttpService {
    async fn handle_request(
        &self,
        request: rmcp::model::ClientRequest,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<rmcp::model::ServerResult, ErrorData> {
        let result = rmcp::Service::handle_request(&self.0, request, context).await?;
        if let rmcp::model::ServerResult::ListToolsResult(list) = result {
            let mut value = serde_json::to_value(list)
                .map_err(|_| ErrorData::internal_error("invalid tool metadata", None))?;
            for tool in value["tools"].as_array_mut().expect("tool array") {
                tool["securitySchemes"] = tool["_meta"]["securitySchemes"].clone();
            }
            Ok(rmcp::model::ServerResult::CustomResult(
                rmcp::model::CustomResult(value),
            ))
        } else {
            Ok(result)
        }
    }
    async fn handle_notification(
        &self,
        notification: rmcp::model::ClientNotification,
        context: rmcp::service::NotificationContext<RoleServer>,
    ) -> std::result::Result<(), ErrorData> {
        rmcp::Service::handle_notification(&self.0, notification, context).await
    }
    fn get_info(&self) -> ServerInfo {
        ServerHandler::get_info(&self.0)
    }
}

impl ServerHandler for HttpTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("meatshell", env!("CARGO_PKG_VERSION")))
            .with_instructions("Access the operator-authorized MeatShell profile. Stored credentials are never returned. OAuth does not override local tool permissions.")
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, ErrorData> {
        let mut definitions = super::tools::definitions();
        for tool in definitions.as_array_mut().expect("tool array") {
            let read_only = matches!(
                tool["name"].as_str(),
                Some(
                    "list_sessions" | "get_session" | "list_remote_files" | "read_remote_text_file"
                )
            );
            if tool.get("annotations").is_none() {
                tool["annotations"] = json!({"readOnlyHint": read_only, "destructiveHint": !read_only, "idempotentHint": read_only, "openWorldHint": !matches!(tool["name"].as_str(), Some("list_sessions" | "get_session"))});
            }
            tool["_meta"] =
                json!({"securitySchemes": [{"type": "oauth2", "scopes": [self.scope]}]});
        }
        let tools = serde_json::from_value(definitions)
            .map_err(|_| ErrorData::internal_error("invalid tool definitions", None))?;
        Ok(ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        let principal = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<Principal>())
            .ok_or_else(|| ErrorData::invalid_request("authentication context missing", None))?;
        let session_cancel = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<SessionCancellation>())
            .ok_or_else(|| ErrorData::invalid_request("session context missing", None))?;
        let remaining = oauth::remaining(principal.expires_at);
        if remaining.is_zero() {
            return Err(ErrorData::invalid_request("access token expired", None));
        }
        let _permit = self
            .calls
            .clone()
            .try_acquire_owned()
            .map_err(|_| ErrorData::internal_error("server busy; retry later", None))?;
        let args = Value::Object(request.arguments.unwrap_or_default());
        let result = tokio::select! {
            biased;
            _ = context.ct.cancelled() => return Err(ErrorData::internal_error("request cancelled", None)),
            _ = session_cancel.0.cancelled() => return Err(ErrorData::internal_error("session closed", None)),
            _ = tokio::time::sleep(remaining) => return Err(ErrorData::internal_error("request deadline or access token expiry reached", None)),
            result = super::tools::call_mcp(&request.name, &args, self.allow_config_import) => result,
        };
        Ok(match result {
            Ok(value) => CallToolResult::structured(value),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(format!("{error:#}"))]),
        })
    }
}

fn failure(status: StatusCode, message: &'static str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}
fn unauthorized(boundary: &Boundary) -> Response {
    let mut response = failure(StatusCode::UNAUTHORIZED, "invalid or missing access token");
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_str(&boundary.oauth.challenge()).expect("validated challenge"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn guard(State(state): State<Arc<Boundary>>, request: Request, next: Next) -> Response {
    let Ok(_permit) = state.requests.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "server busy");
    };
    if request.headers().get_all(header::ORIGIN).iter().count() > 1 {
        return failure(StatusCode::BAD_REQUEST, "ambiguous origin");
    }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        if !origin
            .to_str()
            .is_ok_and(|value| state.origins.iter().any(|allowed| allowed == value))
        {
            return failure(StatusCode::FORBIDDEN, "origin is not allowed");
        }
    }
    if request
        .headers()
        .get_all(header::AUTHORIZATION)
        .iter()
        .count()
        != 1
    {
        return unauthorized(&state);
    }
    let principal = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
        .map(|(_, token)| state.oauth.verify(token));
    let mut principal = match principal {
        Some(Ok(principal)) => principal,
        Some(Err(oauth::AuthError::InsufficientPermission)) => {
            let mut response = failure(
                StatusCode::FORBIDDEN,
                "insufficient scope or subject permission",
            );
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_str(&format!(
                    "{}, error=\"insufficient_scope\"",
                    state.oauth.challenge()
                ))
                .expect("validated challenge"),
            );
            return response;
        }
        _ => return unauthorized(&state),
    };
    if request.method() == Method::GET {
        // This server sends results on POST response streams and does not offer
        // unsolicited server messages. GET=405 is explicitly allowed by MCP.
        let mut response = failure(
            StatusCode::METHOD_NOT_ALLOWED,
            "standalone event stream is not supported",
        );
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST, DELETE"));
        return response;
    }
    if !matches!(*request.method(), Method::POST | Method::DELETE) {
        return failure(StatusCode::METHOD_NOT_ALLOWED, "method is not supported");
    }
    let session = match request.headers().get("mcp-session-id") {
        Some(value) => match value.to_str() {
            Ok(value) if !value.is_empty() && value.len() <= 128 => Some(value.to_owned()),
            _ => return failure(StatusCode::BAD_REQUEST, "invalid session id"),
        },
        None => None,
    };
    if request.headers().get_all("mcp-session-id").iter().count() > 1 {
        return failure(StatusCode::BAD_REQUEST, "ambiguous session id");
    }
    let session_cancel = if let Some(session) = &session {
        let owners = state.owners.lock().await;
        let Some(owner) = owners
            .get(session)
            .filter(|owner| owner.subject == principal.subject && owner.expires_at > oauth::now())
        else {
            return failure(
                StatusCode::NOT_FOUND,
                "MCP session not found; initialize again",
            );
        };
        principal.expires_at = principal.expires_at.min(owner.expires_at);
        Some(SessionCancellation(owner.cancellation.clone()))
    } else {
        None
    };
    let method = request.method().clone();
    let (mut parts, body) = request.into_parts();
    let body = match tokio::time::timeout(Duration::from_secs(5), to_bytes(body, MAX_BODY)).await {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return failure(StatusCode::PAYLOAD_TOO_LARGE, "request body exceeds limit"),
        Err(_) => return failure(StatusCode::REQUEST_TIMEOUT, "request body timed out"),
    };
    let parsed = if method == Method::POST {
        match serde_json::from_slice::<Value>(&body) {
            Ok(value) if value.is_object() => Some(value),
            _ => return failure(StatusCode::BAD_REQUEST, "invalid JSON-RPC object"),
        }
    } else {
        None
    };
    let has_id = parsed
        .as_ref()
        .is_some_and(|value| value.get("id").is_some());
    // Hold through the full response body, while notifications/DELETE retain
    // separate admission capacity to cancel work under load.
    let stream_permit = if has_id {
        match state.streams.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                return failure(
                    StatusCode::TOO_MANY_REQUESTS,
                    "too many active response streams",
                )
            }
        }
    } else {
        None
    };
    // Serialize creation with cleanup so a timed-out initialization cannot
    // leave an unowned SDK session alive forever.
    let _initialize = if session.is_none() {
        if method != Method::POST {
            return failure(StatusCode::BAD_REQUEST, "session id required");
        }
        if !parsed
            .as_ref()
            .is_some_and(|value| value["method"] == "initialize")
        {
            return failure(StatusCode::BAD_REQUEST, "initialize first");
        }
        let lock = state.initialize_lock.lock().await;
        if state.sessions.sessions.read().await.len() >= MAX_SESSIONS {
            return failure(
                StatusCode::TOO_MANY_REQUESTS,
                "session limit reached; retry after closing a session",
            );
        }
        Some(lock)
    } else {
        None
    };
    parts.extensions.insert(principal.clone());
    if let Some(cancel) = session_cancel {
        parts.extensions.insert(cancel);
    }
    // Do not propagate bearer tokens into the SDK's request context/debug view.
    parts.headers.remove(header::AUTHORIZATION);
    let request = Request::from_parts(parts, Body::from(body));
    let mut response = match tokio::time::timeout(Duration::from_secs(10), next.run(request)).await
    {
        Ok(response) => response,
        Err(_) => return failure(StatusCode::GATEWAY_TIMEOUT, "protocol response timed out"),
    };
    if session.is_none() && response.status().is_success() {
        if let Some(id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            state.owners.lock().await.insert(
                id.to_owned(),
                SessionOwner {
                    subject: principal.subject,
                    expires_at: oauth::now() + SESSION_TTL,
                    cancellation: CancellationToken::new(),
                },
            );
        }
    } else if method == Method::DELETE && response.status().is_success() {
        if let Some(session) = session {
            state.owners.lock().await.remove(&session);
        }
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let (parts, body) = response.into_parts();
    let remaining = oauth::remaining(principal.expires_at);
    // Also bound the response stream; a token cannot keep reading after expiry.
    let stream = body
        .into_data_stream()
        .take_until(tokio::time::sleep(remaining))
        .map(move |chunk| {
            let _hold = &stream_permit;
            chunk
        });
    Response::from_parts(parts, Body::from_stream(stream))
}

async fn cleanup(state: &Boundary, all: bool) {
    let _initialize = state.initialize_lock.lock().await;
    let ids: Vec<_> = state
        .sessions
        .sessions
        .read()
        .await
        .keys()
        .cloned()
        .collect();
    state.owners.lock().await.retain(|id, owner| {
        !all && owner.expires_at > oauth::now() && ids.iter().any(|live| live.as_ref() == id)
    });
    for id in ids {
        let stale = all
            || !state
                .owners
                .lock()
                .await
                .get(id.as_ref())
                .is_some_and(|owner| owner.expires_at > oauth::now());
        if stale {
            let _ = tokio::time::timeout(Duration::from_secs(2), state.sessions.close_session(&id))
                .await;
            state.owners.lock().await.remove(id.as_ref());
        }
    }
}

pub(super) fn run(path: &str, allow_config_import: bool) -> Result<()> {
    ensure!(
        std::fs::metadata(path)
            .context("read HTTP config metadata")?
            .len()
            <= 64 * 1024,
        "HTTP config exceeds 64 KiB"
    );
    let config: HttpConfig =
        serde_json::from_slice(&std::fs::read(path).context("read HTTP config")?)
            .context("parse HTTP config")?;
    let base = Path::new(path).parent().unwrap_or_else(|| Path::new("."));
    let oauth = OAuth::load(config.oauth, base)?;
    let resource = oauth::https_url(&oauth.config.resource)?;
    let public_authority = resource[url::Position::BeforeHost..url::Position::AfterPort].to_owned();
    ensure!(
        config.allowed_origins.len() <= 32,
        "too many allowed origins"
    );
    for origin in &config.allowed_origins {
        let url = oauth::https_url(origin)?;
        ensure!(
            url.origin().ascii_serialization() == *origin,
            "allowed_origins require exact HTTPS origins without paths"
        );
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("create HTTP runtime")?;
    runtime.block_on(async move {
        let mut manager = LocalSessionManager::default();
        manager.session_config.completed_cache_ttl = Duration::ZERO;
        let sessions = Arc::new(manager);
        let state = Arc::new(Boundary { oauth, origins: config.allowed_origins, owners: Mutex::new(HashMap::new()),
            initialize_lock: Mutex::new(()), sessions: sessions.clone(), requests: Semaphore::new(32), streams: Arc::new(Semaphore::new(16)) });
        let shutdown = CancellationToken::new();
        let tools = HttpTools { allow_config_import, scope: state.oauth.config.required_scope.clone(), calls: Arc::new(Semaphore::new(16)) };
        let service = StreamableHttpService::new(move || Ok(HttpService(tools.clone())), sessions,
            StreamableHttpServerConfig::default().with_allowed_hosts([public_authority, config.bind.to_string()])
                .with_cancellation_token(shutdown.child_token()));
        let mcp = Router::new().nest_service("/mcp", service)
            .layer(middleware::from_fn_with_state(state.clone(), guard));
        let metadata = state.oauth.metadata();
        let metadata2 = metadata.clone();
        let router = mcp
            .route("/.well-known/oauth-protected-resource/mcp", get(move || async move { Json(metadata) }))
            .route("/.well-known/oauth-protected-resource", get(move || async move { Json(metadata2) }));
        let listener = tokio::net::TcpListener::bind(config.bind).await.context("bind authenticated MCP listener")?;
        eprintln!("MeatShell authenticated MCP listening on http://{}/mcp; HTTPS reverse proxy required", listener.local_addr()?);
        let reap_state = state.clone();
        let reap_stop = shutdown.clone();
        let reaper = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = reap_stop.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(30)) => cleanup(&reap_state, false).await,
                }
            }
        });
        let signal = shutdown.clone();
        let result = axum::serve(listener, router).with_graceful_shutdown(async move {
            #[cfg(unix)] {
                if let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
                } else { let _ = tokio::signal::ctrl_c().await; }
            }
            #[cfg(not(unix))] { let _ = tokio::signal::ctrl_c().await; }
            signal.cancel();
        }).await;
        shutdown.cancel();
        reaper.abort();
        cleanup(&state, true).await;
        result.context("serve authenticated MCP")
    })
}
