use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderValue, Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, patch, post},
};
use lumiere_proto::{
    Animation, AnimationId, AnimationSummary, ClientMsg, CommandRequest, CommandResponse, LightId,
    PlaybackOptions, PlaybackStatus, Preset, PresetId, ResyncReason, Selector, ServerMsg,
    TargetBinding, WS_PROTOCOL_VERSION, WorldSnapshot,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::{sync::broadcast::error::TryRecvError, time::Instant};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    limit::RequestBodyLimitLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use crate::{RegistryHandle, config::Config};

mod static_assets;

const MAX_BODY_BYTES: usize = 256 * 1024;
const REST_TIMEOUT: Duration = Duration::from_secs(10);
const TICKET_LIFETIME: Duration = Duration::from_secs(30);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Shared state for HTTP and WebSocket handlers.
#[derive(Clone)]
pub struct ApiState {
    registry: RegistryHandle,
    require_token: bool,
    token: Arc<str>,
    cors_origins: Arc<[String]>,
    session: Arc<str>,
    tickets: Arc<Mutex<HashMap<String, Instant>>>,
}

impl ApiState {
    pub fn new(registry: RegistryHandle, config: &Config) -> Self {
        Self {
            registry,
            token: Arc::from(config.token.as_str()),
            require_token: true,
            cors_origins: Arc::from(config.cors_origins.clone()),
            session: Arc::from(random_hex(16)),
            tickets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Turns off bearer-token checks for every route. An explicit runtime
    /// choice (--disable-authentication), never a persisted default.
    pub fn without_authentication(mut self) -> Self {
        self.require_token = false;
        self
    }

    /// Returns the process-local session identifier.
    pub fn session(&self) -> &str {
        &self.session
    }
}

/// Builds the daemon HTTP and WebSocket router.
pub fn router(state: ApiState) -> Router {
    let rest = Router::new()
        .route("/scan", post(scan))
        .route("/lights", get(lights))
        .route("/lights/{id}", patch(set_label))
        .route("/lights/{id}/connect", post(connect))
        .route("/lights/{id}/disconnect", post(disconnect))
        .route("/command", post(command))
        .route("/animations", get(animations))
        .route("/animations/{id}", get(animation))
        .route("/animations/{id}/play", post(play_animation))
        .route("/playback/stop", post(stop_playback))
        .route("/presets", get(presets).post(capture_preset))
        .route("/presets/{id}", patch(rename_preset).delete(delete_preset))
        .route("/presets/{id}/recall", post(recall_preset))
        .route("/presets/{id}/capture", post(recapture_preset))
        .route("/ws-ticket", post(ws_ticket))
        .fallback(api_not_found)
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REST_TIMEOUT,
        ))
        .layer(TraceLayer::new_for_http());

    let rest = if state.cors_origins.is_empty() {
        rest.layer(CorsLayer::new())
    } else {
        let origins = state
            .cors_origins
            .iter()
            .filter_map(|origin| HeaderValue::from_str(origin).ok())
            .collect::<Vec<_>>();
        rest.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
        )
    };

    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/events", get(events))
        .nest("/api/v1", rest)
        .route("/api/{*path}", any(api_not_found))
        .fallback_service(static_assets::router());
    #[cfg(not(feature = "embed-ui"))]
    let router = router.layer(middleware::from_fn(no_cache_index));
    router.with_state(state)
}

#[cfg(not(feature = "embed-ui"))]
async fn no_cache_index(request: Request<Body>, next: Next) -> Response {
    let may_serve_static =
        request.uri().path() != "/healthz" && !request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    if may_serve_static && is_html && response.status().is_success() {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    response
}

async fn healthz() -> &'static str {
    "ok"
}

#[derive(Deserialize)]
struct ScanRequest {
    duration_ms: u64,
}

async fn scan(
    State(state): State<ApiState>,
    Json(request): Json<ScanRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state
        .registry
        .discover(Duration::from_millis(request.duration_ms))
        .await
        .map_err(ApiError::registry)?;
    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({}))))
}

async fn lights(State(state): State<ApiState>) -> Json<WorldSnapshot> {
    Json(state.registry.world().borrow().as_ref().clone())
}

async fn connect(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .registry
        .connect(parse_id(&id)?)
        .await
        .map_err(ApiError::gateway)?;
    Ok(Json(serde_json::json!({})))
}

async fn disconnect(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .registry
        .disconnect(parse_id(&id)?)
        .await
        .map_err(ApiError::gateway)?;
    Ok(Json(serde_json::json!({})))
}

#[derive(Deserialize)]
struct LabelRequest {
    label: String,
}

async fn set_label(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<LabelRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let light = state
        .registry
        .set_label(parse_id(&id)?, request.label)
        .await
        .map_err(ApiError::not_found)?;
    Ok(Json(light))
}

async fn command(
    State(state): State<ApiState>,
    Json(request): Json<CommandRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let results = state
        .registry
        .set_mode(request.selector, request.mode, request.wait)
        .await
        .map_err(ApiError::registry)?;
    Ok(Json(CommandResponse { results }))
}

async fn animations(
    State(state): State<ApiState>,
) -> Result<Json<Vec<AnimationSummary>>, ApiError> {
    state
        .registry
        .list_animations()
        .await
        .map(Json)
        .map_err(ApiError::registry)
}

async fn animation(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<Animation>, ApiError> {
    let id = parse_animation_id(&id)?;
    state
        .registry
        .animation(id)
        .await
        .map_err(ApiError::registry)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("animation was not found".to_owned()))
}

#[derive(Default, Deserialize)]
struct PlayRequest {
    #[serde(default)]
    options: PlaybackOptions,
    #[serde(default)]
    binding: TargetBinding,
}

async fn play_animation(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<PlayRequest>,
) -> Result<Json<PlaybackStatus>, ApiError> {
    let id = parse_animation_id(&id)?;
    if state
        .registry
        .animation(id.clone())
        .await
        .map_err(ApiError::registry)?
        .is_none()
    {
        return Err(ApiError::not_found(format!("animation {id} was not found")));
    }
    state
        .registry
        .play(id, request.options, request.binding)
        .await
        .map(Json)
        .map_err(ApiError::conflict)
}

async fn stop_playback(State(state): State<ApiState>) -> Result<Json<serde_json::Value>, ApiError> {
    let stopped = state
        .registry
        .stop_playback()
        .await
        .map_err(ApiError::registry)?;
    Ok(Json(serde_json::json!({ "stopped": stopped })))
}

async fn presets(State(state): State<ApiState>) -> Result<Json<Vec<Preset>>, ApiError> {
    state
        .registry
        .list_presets()
        .await
        .map(Json)
        .map_err(ApiError::registry)
}

#[derive(Deserialize)]
struct CapturePresetRequest {
    name: String,
    selector: Selector,
}

async fn capture_preset(
    State(state): State<ApiState>,
    Json(request): Json<CapturePresetRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let preset = state
        .registry
        .save_preset(request.name, request.selector)
        .await
        .map_err(ApiError::conflict)?;
    Ok((StatusCode::CREATED, Json(preset)))
}

#[derive(Default, Deserialize)]
struct RecallPresetRequest {
    #[serde(default)]
    wait: bool,
}

async fn recall_preset(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<RecallPresetRequest>,
) -> Result<Json<CommandResponse>, ApiError> {
    let id = parse_preset_id(&id)?;
    require_preset(&state, &id).await?;
    let results = state
        .registry
        .recall_preset(id, request.wait)
        .await
        .map_err(ApiError::registry)?;
    Ok(Json(CommandResponse { results }))
}

#[derive(Deserialize)]
struct RecapturePresetRequest {
    #[serde(default)]
    selector: Option<Selector>,
}

/// Overwrites a preset's entries with a fresh capture, keeping name and id.
async fn recapture_preset(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<RecapturePresetRequest>,
) -> Result<Json<Preset>, ApiError> {
    let id = parse_preset_id(&id)?;
    require_preset(&state, &id).await?;
    let selector = request.selector.unwrap_or(Selector::All);
    let preset = state
        .registry
        .recapture_preset(id, selector)
        .await
        .map_err(ApiError::registry)?;
    Ok(Json(preset))
}

#[derive(Deserialize)]
struct RenamePresetRequest {
    name: String,
}

async fn rename_preset(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<RenamePresetRequest>,
) -> Result<Json<Preset>, ApiError> {
    let id = parse_preset_id(&id)?;
    require_preset(&state, &id).await?;
    state
        .registry
        .rename_preset(id.clone(), request.name)
        .await
        .map_err(ApiError::conflict)?;
    require_preset(&state, &id).await.map(Json)
}

async fn delete_preset(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_preset_id(&id)?;
    require_preset(&state, &id).await?;
    state
        .registry
        .delete_preset(id)
        .await
        .map_err(ApiError::registry)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn require_preset(state: &ApiState, id: &PresetId) -> Result<Preset, ApiError> {
    state
        .registry
        .list_presets()
        .await
        .map_err(ApiError::registry)?
        .into_iter()
        .find(|preset| &preset.id == id)
        .ok_or_else(|| ApiError::not_found(format!("preset {id} was not found")))
}

#[derive(Serialize)]
struct TicketResponse {
    ticket: String,
    expires_ms: u64,
}

async fn ws_ticket(State(state): State<ApiState>) -> Json<TicketResponse> {
    let ticket = random_hex(16);
    let now = Instant::now();
    let mut tickets = state.tickets.lock().expect("ticket mutex poisoned");
    tickets.retain(|_, expiry| *expiry > now);
    tickets.insert(ticket.clone(), now + TICKET_LIFETIME);
    Json(TicketResponse {
        ticket,
        expires_ms: TICKET_LIFETIME.as_millis() as u64,
    })
}

async fn events(State(state): State<ApiState>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| websocket(state, socket))
}

async fn websocket(state: ApiState, mut socket: WebSocket) {
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str::<ClientMsg>(&text).ok(),
        _ => None,
    };
    let Some(ClientMsg::Hello {
        protocol_version,
        ticket,
        last_seq,
    }) = hello
    else {
        send_error_and_close(&mut socket, "expected hello").await;
        return;
    };
    if protocol_version != WS_PROTOCOL_VERSION {
        send_error_and_close(&mut socket, "unsupported protocol version").await;
        return;
    }
    if !consume_ticket(&state, &ticket) {
        send_error_and_close(&mut socket, "invalid or expired ticket").await;
        return;
    }

    let mut events = state.registry.events();
    let world = state.registry.world().borrow().as_ref().clone();
    // A last_seq beyond our current seq means the client outlived a daemon
    // restart: its numbering is from another life, so force a snapshot.
    let replay = last_seq
        .filter(|seq| *seq <= world.seq)
        .and_then(|seq| state.registry.events_since(seq));
    let snapshot = if replay.is_some() {
        None
    } else {
        Some(world.clone())
    };
    let welcome = ServerMsg::Welcome {
        protocol_version: WS_PROTOCOL_VERSION,
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
        session: state.session.to_string(),
        seq: world.seq,
        snapshot,
    };
    if !send_json(&mut socket, &welcome).await {
        return;
    }
    // Start the duplicate filter past everything already delivered: events
    // emitted between the world read and the ring read appear in the replay
    // AND in the broadcast queue.
    let mut last_seq = world.seq;
    if let Some(replay) = replay {
        if let Some(newest) = replay.last() {
            last_seq = last_seq.max(newest.seq);
        }
        if !send_json(&mut socket, &ServerMsg::Patch { events: replay }).await {
            return;
        }
    }

    let mut last_sent = Instant::now();
    let mut missed_pongs = 0_u8;
    let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
    keepalive.tick().await;

    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(ClientMsg::Ping { nonce }) = serde_json::from_str(&text) {
                        if !send_json(&mut socket, &ServerMsg::Pong { nonce }).await {
                            break;
                        }
                        last_sent = Instant::now();
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    if socket.send(Message::Pong(payload)).await.is_err() {
                        break;
                    }
                    last_sent = Instant::now();
                }
                Some(Ok(Message::Pong(_))) => missed_pongs = 0,
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(Message::Binary(_))) => {}
            },
            event = events.recv() => match event {
                Ok(event) => {
                    if event.seq <= last_seq {
                        continue;
                    }
                    let mut batch = vec![event];
                    let mut lagged = false;
                    loop {
                        match events.try_recv() {
                            Ok(event) => batch.push(event),
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Lagged(_)) => {
                                lagged = true;
                                break;
                            }
                            Err(TryRecvError::Closed) => return,
                        }
                    }
                    if lagged {
                        let snapshot = state.registry.world().borrow().as_ref().clone();
                        last_seq = snapshot.seq;
                        if !send_json(&mut socket, &ServerMsg::Resync {
                            snapshot,
                            reason: ResyncReason::ClientLagged,
                        }).await {
                            break;
                        }
                    } else {
                        last_seq = batch.last().map_or(last_seq, |event| event.seq);
                        if !send_json(&mut socket, &ServerMsg::Patch { events: batch }).await {
                            break;
                        }
                    }
                    last_sent = Instant::now();
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let snapshot = state.registry.world().borrow().as_ref().clone();
                    last_seq = snapshot.seq;
                    if !send_json(&mut socket, &ServerMsg::Resync {
                        snapshot,
                        reason: ResyncReason::ClientLagged,
                    }).await {
                        break;
                    }
                    last_sent = Instant::now();
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            _ = keepalive.tick() => {
                if last_sent.elapsed() < KEEPALIVE_INTERVAL {
                    continue;
                }
                if missed_pongs >= 2 {
                    break;
                }
                if socket.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
                missed_pongs += 1;
                last_sent = Instant::now();
            }
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

fn consume_ticket(state: &ApiState, ticket: &str) -> bool {
    let now = Instant::now();
    state
        .tickets
        .lock()
        .expect("ticket mutex poisoned")
        .remove(ticket)
        .is_some_and(|expiry| expiry > now)
}

async fn send_json(socket: &mut WebSocket, message: &ServerMsg) -> bool {
    let Ok(encoded) = serde_json::to_string(message) else {
        return false;
    };
    socket.send(Message::Text(encoded.into())).await.is_ok()
}

async fn send_error_and_close(socket: &mut WebSocket, message: &str) {
    let _ = send_json(
        socket,
        &ServerMsg::Error {
            message: message.to_owned(),
        },
    )
    .await;
    let _ = socket.send(Message::Close(None)).await;
}

async fn authorize(State(state): State<ApiState>, request: Request<Body>, next: Next) -> Response {
    if !state.require_token {
        return next.run(request).await;
    }
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| {
            token.len() == state.token.len()
                && token.as_bytes().ct_eq(state.token.as_bytes()).into()
        });
    if authorized {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response()
    }
}

fn parse_id(value: &str) -> Result<LightId, ApiError> {
    LightId::parse(value).map_err(|error| ApiError {
        status: StatusCode::BAD_REQUEST,
        message: error.to_string(),
    })
}

fn parse_animation_id(value: &str) -> Result<AnimationId, ApiError> {
    AnimationId::parse(value).map_err(|message| ApiError {
        status: StatusCode::BAD_REQUEST,
        message,
    })
}

fn parse_preset_id(value: &str) -> Result<PresetId, ApiError> {
    PresetId::parse(value).map_err(|message| ApiError {
        status: StatusCode::BAD_REQUEST,
        message,
    })
}

async fn api_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "not found"})),
    )
        .into_response()
}

fn random_hex(length: usize) -> String {
    let mut bytes = vec![0_u8; length];
    getrandom::fill(&mut bytes).expect("operating-system random generator failed");
    let mut encoded = String::with_capacity(length * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn registry(message: String) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message,
        }
    }

    fn gateway(message: String) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message,
        }
    }

    fn not_found(message: String) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message,
        }
    }

    fn conflict(message: String) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({"error": self.message})),
        )
            .into_response()
    }
}
