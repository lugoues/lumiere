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
    ClientMsg, CommandRequest, CommandResponse, LightId, ResyncReason, ServerMsg,
    WS_PROTOCOL_VERSION, WorldSnapshot,
};
use rand::{TryRngCore, rngs::OsRng};
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

const MAX_BODY_BYTES: usize = 256 * 1024;
const REST_TIMEOUT: Duration = Duration::from_secs(10);
const TICKET_LIFETIME: Duration = Duration::from_secs(30);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Shared state for HTTP and WebSocket handlers.
#[derive(Clone)]
pub struct ApiState {
    registry: RegistryHandle,
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
            cors_origins: Arc::from(config.cors_origins.clone()),
            session: Arc::from(random_hex(16)),
            tickets: Arc::new(Mutex::new(HashMap::new())),
        }
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
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
        )
    };

    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/events", get(events))
        .nest("/api/v1", rest)
        .route("/api/{*path}", any(api_not_found))
        .fallback(spa)
        .with_state(state)
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
    let replay = last_seq.and_then(|seq| state.registry.events_since(seq));
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
    if let Some(replay) = replay
        && !send_json(&mut socket, &ServerMsg::Patch { events: replay }).await
    {
        return;
    }

    let mut last_seq = world.seq;
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

async fn api_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "not found"})),
    )
        .into_response()
}

async fn spa() -> Response {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dist/web/index.html");
    match std::fs::read_to_string(path) {
        Ok(index) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            index,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn random_hex(length: usize) -> String {
    let mut bytes = vec![0_u8; length];
    OsRng
        .try_fill_bytes(&mut bytes)
        .expect("operating-system random generator failed");
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
