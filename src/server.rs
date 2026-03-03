use crate::{
    Actor, Engine, EngineConfig, Event, EventPatch, ReservationStatus, Role, SlotForgeError,
    SqliteEngine, StoreError, WaitlistPolicy,
};
use axum::{
    extract::{Path, State},
    http::{header::SET_COOKIE, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
pub struct AppState {
    store: Arc<Mutex<SqliteEngine>>,
    actors: Arc<BTreeMap<String, Actor>>,
}

#[derive(Serialize)]
struct PublicEvent {
    #[serde(flatten)]
    event: Event,
    confirmed_count: u32,
    remaining_capacity: u32,
}

#[derive(Serialize)]
struct ApiError {
    error: ApiErrorBody,
}
#[derive(Serialize)]
struct ApiErrorBody {
    code: &'static str,
    message: String,
    retryable: bool,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = match self.error.code {
            "NOT_FOUND" => StatusCode::NOT_FOUND,
            "FORBIDDEN" => StatusCode::FORBIDDEN,
            "IDEMPOTENCY_CONFLICT" | "REVISION_CONFLICT" => StatusCode::CONFLICT,
            "STORE_UNAVAILABLE" | "STORE_CORRUPT" => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        };
        (status, Json(self)).into_response()
    }
}

#[derive(Deserialize)]
struct ReserveRequest {
    event_id: String,
    cohort: String,
}

#[derive(Deserialize)]
struct CapacityRequest {
    capacity: u32,
}

#[derive(Deserialize)]
struct UpdateEventRequest {
    title: Option<String>,
    capacity: Option<u32>,
    starts_at: Option<i64>,
    ends_at: Option<i64>,
    booking_close_at: Option<i64>,
    active: Option<bool>,
}

impl From<UpdateEventRequest> for EventPatch {
    fn from(value: UpdateEventRequest) -> Self {
        Self {
            title: value.title,
            capacity: value.capacity,
            starts_at: value.starts_at,
            ends_at: value.ends_at,
            booking_close_at: value.booking_close_at,
            active: value.active,
        }
    }
}

#[derive(Deserialize)]
struct CreateEventRequest {
    event_id: String,
    title: String,
    capacity: u32,
    starts_at: i64,
    ends_at: i64,
    booking_close_at: i64,
}

#[derive(Deserialize)]
struct DemoSessionRequest {
    actor_key: String,
}

#[derive(Deserialize)]
struct RelayFaultRequest {
    mode: String,
}

#[derive(Serialize)]
struct DemoSessionResponse {
    actor_id: String,
    role: Role,
}

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn app(runtime_dir: PathBuf) -> Result<Router, String> {
    std::fs::create_dir_all(&runtime_dir).map_err(|error| error.to_string())?;
    let now = now_seconds();
    let initial = Engine::new(
        vec![Event::with_booking_close(
            "workshop-a",
            "Local booking workshop",
            2,
            now + 3600,
            now + 7200,
            now + 1800,
        )
        .map_err(|error| error.to_string())?],
        EngineConfig {
            max_active_per_attendee: 3,
            policy: WaitlistPolicy::Fifo,
        },
    )
    .map_err(|error| error.to_string())?;
    let store = SqliteEngine::open(&runtime_dir.join("slotforge.sqlite"), initial)
        .map_err(|error| error.to_string())?;
    let mut actors = BTreeMap::new();
    for (id, role) in [
        ("demo-a", Role::Attendee),
        ("demo-b", Role::Attendee),
        ("demo-c", Role::Attendee),
        ("demo-coordinator", Role::Coordinator),
    ] {
        let actor = match role {
            Role::Attendee => Actor::attendee(id),
            Role::Coordinator => Actor::coordinator(id),
            Role::Observer => Actor::observer(id),
        }
        .map_err(|error| error.to_string())?;
        actors.insert(id.to_string(), actor);
    }
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
        actors: Arc::new(actors),
    };
    let router = Router::new()
        .route(
            "/api/v1/health",
            get(|| async { Json(serde_json::json!({"status":"ok"})) }),
        )
        .route("/api/v1/events", get(events).post(create_event))
        .route("/api/v1/events/{event_id}", get(event).patch(update_event))
        .route("/api/v1/demo/session", post(demo_session))
        .route("/api/v1/reservations", post(reserve))
        .route("/api/v1/me/reservations", get(my_reservations))
        .route("/api/v1/me/notifications", get(my_notifications))
        .route(
            "/api/v1/notifications/{notification_id}/read",
            patch(mark_notification_read),
        )
        .route("/api/v1/reservations/{reservation_id}/cancel", post(cancel))
        .route(
            "/api/v1/reservations/{reservation_id}/check-in",
            post(check_in),
        )
        .route("/api/v1/admin/events/{event_id}", patch(set_capacity))
        .route("/api/v1/admin/events/{event_id}/roster", get(roster))
        .route("/api/v1/admin/deliveries", get(deliveries))
        .route(
            "/api/v1/admin/deliveries/{event_id}/retry",
            post(retry_delivery),
        )
        .route(
            "/api/v1/admin/demo/relay-fault",
            post(configure_relay_fault),
        )
        .with_state(state);
    let web_dist = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/dist");
    if web_dist.join("index.html").is_file() {
        Ok(router.fallback_service(
            ServeDir::new(&web_dist).not_found_service(ServeFile::new(web_dist.join("index.html"))),
        ))
    } else {
        Ok(router)
    }
}

async fn events(State(state): State<AppState>) -> Result<Json<Vec<PublicEvent>>, ApiError> {
    let store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_LOCK", "local command lock poisoned"))?;
    let engine = store.engine();
    let events = engine
        .events()
        .map(|event| {
            let confirmed_count = engine
                .reservations()
                .filter(|reservation| {
                    reservation.event_id == event.event_id
                        && matches!(
                            reservation.status,
                            ReservationStatus::Confirmed | ReservationStatus::CheckedIn
                        )
                })
                .count()
                .min(u32::MAX as usize) as u32;
            PublicEvent {
                event: event.clone(),
                confirmed_count,
                remaining_capacity: event.capacity.saturating_sub(confirmed_count),
            }
        })
        .collect();
    Ok(Json(events))
}

async fn event(
    Path(event_id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<crate::Event>, ApiError> {
    let store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_LOCK", "local command lock poisoned"))?;
    let found = store
        .engine()
        .events()
        .find(|event| event.event_id == event_id)
        .cloned();
    found
        .map(Json)
        .ok_or_else(|| failure("NOT_FOUND", "event not found"))
}

async fn reserve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ReserveRequest>,
) -> Result<Json<crate::ReservationReceipt>, ApiError> {
    let actor = authenticated(&state, &headers)?;
    let request_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| failure("INVALID_REQUEST", "Idempotency-Key is required"))?;
    let mut store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_LOCK", "local command lock poisoned"))?;
    store
        .reserve(
            &actor,
            request_key,
            &request.event_id,
            &request.cohort,
            now_seconds(),
        )
        .map(Json)
        .map_err(map_store)
}

async fn create_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateEventRequest>,
) -> Result<(StatusCode, Json<crate::Event>), ApiError> {
    let actor = authenticated(&state, &headers)?;
    let request_key = idempotency_key(&headers)?;
    let event = Event::with_booking_close(
        request.event_id,
        request.title,
        request.capacity,
        request.starts_at,
        request.ends_at,
        request.booking_close_at,
    )
    .map_err(map_domain)?;
    let mut store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_LOCK", "local command lock poisoned"))?;
    let created = store
        .create_event(&actor, request_key, event, now_seconds())
        .map_err(map_store)?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn my_reservations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<crate::Reservation>>, ApiError> {
    let actor = authenticated(&state, &headers)?;
    let store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_LOCK", "local command lock poisoned"))?;
    Ok(Json(
        store
            .engine()
            .reservations()
            .filter(|reservation| reservation.attendee_id == actor.actor_id)
            .cloned()
            .collect(),
    ))
}

async fn my_notifications(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<crate::Notification>>, ApiError> {
    let actor = authenticated(&state, &headers)?;
    let store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_UNAVAILABLE", "local command lock poisoned"))?;
    store.notifications(&actor).map(Json).map_err(map_store)
}

async fn mark_notification_read(
    Path(notification_id): Path<i64>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let actor = authenticated(&state, &headers)?;
    let store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_UNAVAILABLE", "local command lock poisoned"))?;
    if store
        .mark_notification_read(&actor, notification_id, now_seconds())
        .map_err(map_store)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(failure(
            "NOT_FOUND",
            "notification not found for this identity",
        ))
    }
}

async fn cancel(
    Path(reservation_id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<crate::CancellationReceipt>, ApiError> {
    let actor = authenticated(&state, &headers)?;
    let request_key = idempotency_key(&headers)?;
    let mut store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_LOCK", "local command lock poisoned"))?;
    store
        .cancel(&actor, request_key, reservation_id, now_seconds())
        .map(Json)
        .map_err(map_store)
}

async fn check_in(
    Path(reservation_id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<crate::CheckInReceipt>, ApiError> {
    let actor = authenticated(&state, &headers)?;
    let request_key = idempotency_key(&headers)?;
    let mut store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_LOCK", "local command lock poisoned"))?;
    store
        .check_in(&actor, request_key, reservation_id, now_seconds())
        .map(Json)
        .map_err(map_store)
}

async fn set_capacity(
    Path(event_id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CapacityRequest>,
) -> Result<Json<crate::EventUpdateReceipt>, ApiError> {
    let actor = authenticated(&state, &headers)?;
    let request_key = idempotency_key(&headers)?;
    let expected_revision = if_match(&headers)?;
    let mut store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_LOCK", "local command lock poisoned"))?;
    store
        .update_event(
            &actor,
            request_key,
            &event_id,
            expected_revision,
            EventPatch {
                capacity: Some(request.capacity),
                ..EventPatch::default()
            },
            now_seconds(),
        )
        .map(Json)
        .map_err(map_store)
}

async fn update_event(
    Path(event_id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UpdateEventRequest>,
) -> Result<Json<crate::EventUpdateReceipt>, ApiError> {
    let actor = authenticated(&state, &headers)?;
    let request_key = idempotency_key(&headers)?;
    let expected_revision = if_match(&headers)?;
    let mut store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_UNAVAILABLE", "local command lock poisoned"))?;
    store
        .update_event(
            &actor,
            request_key,
            &event_id,
            expected_revision,
            request.into(),
            now_seconds(),
        )
        .map(Json)
        .map_err(map_store)
}

async fn roster(
    Path(event_id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<crate::Reservation>>, ApiError> {
    let actor = authenticated(&state, &headers)?;
    if actor.role != Role::Coordinator {
        return Err(failure("FORBIDDEN", "coordinator role is required"));
    }
    let store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_LOCK", "local command lock poisoned"))?;
    Ok(Json(
        store
            .engine()
            .reservations()
            .filter(|reservation| reservation.event_id == event_id)
            .cloned()
            .collect(),
    ))
}

async fn deliveries(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<crate::Delivery>>, ApiError> {
    let actor = authenticated(&state, &headers)?;
    if actor.role != Role::Coordinator {
        return Err(failure("FORBIDDEN", "coordinator role is required"));
    }
    let store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_UNAVAILABLE", "local command lock poisoned"))?;
    store.deliveries().map(Json).map_err(map_store)
}

async fn retry_delivery(
    Path(event_id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let actor = authenticated(&state, &headers)?;
    if actor.role != Role::Coordinator {
        return Err(failure("FORBIDDEN", "coordinator role is required"));
    }
    let store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_UNAVAILABLE", "local command lock poisoned"))?;
    if store
        .retry_delivery(&event_id, now_seconds())
        .map_err(map_store)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(failure(
            "INVALID_COMMAND",
            "delivery is not a dead-letter record",
        ))
    }
}

async fn configure_relay_fault(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RelayFaultRequest>,
) -> Result<StatusCode, ApiError> {
    let actor = authenticated(&state, &headers)?;
    if actor.role != Role::Coordinator {
        return Err(failure("FORBIDDEN", "coordinator role is required"));
    }
    if !matches!(
        request.mode.as_str(),
        "before_inbox" | "after_inbox_before_ack"
    ) {
        return Err(failure("INVALID_COMMAND", "unknown relay fault mode"));
    }
    state
        .store
        .lock()
        .map_err(|_| failure("STORE_UNAVAILABLE", "local command lock poisoned"))?
        .configure_relay_fault(&request.mode)
        .map_err(map_store)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn demo_session(
    State(state): State<AppState>,
    Json(request): Json<DemoSessionRequest>,
) -> Result<(HeaderMap, Json<DemoSessionResponse>), ApiError> {
    let actor = state
        .actors
        .get(&request.actor_key)
        .ok_or_else(|| failure("FORBIDDEN", "unknown local demo identity"))?
        .clone();
    let nonce = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let token = format!("{}-{}-{nonce}", actor.actor_id, now_seconds());
    let hash = sha256_hex(token.as_bytes());
    let expires_at = now_seconds() + 8 * 60 * 60;
    state
        .store
        .lock()
        .map_err(|_| failure("STORE_LOCK", "local command lock poisoned"))?
        .save_session(&hash, &actor.actor_id, expires_at)
        .map_err(map_store)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        HeaderValue::from_str(&format!(
            "slotforge_session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=28800"
        ))
        .map_err(|_| failure("INVALID_SESSION", "cannot encode session"))?,
    );
    Ok((
        headers,
        Json(DemoSessionResponse {
            actor_id: actor.actor_id,
            role: actor.role,
        }),
    ))
}

fn authenticated(state: &AppState, headers: &HeaderMap) -> Result<Actor, ApiError> {
    let token = headers
        .get("cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|cookie| {
            cookie
                .split(';')
                .map(str::trim)
                .find_map(|part| part.strip_prefix("slotforge_session="))
        })
        .ok_or_else(|| failure("FORBIDDEN", "local demo session is required"))?;
    let hash = sha256_hex(token.as_bytes());
    let store = state
        .store
        .lock()
        .map_err(|_| failure("STORE_LOCK", "local command lock poisoned"))?;
    let actor_id = store
        .session_actor(&hash, now_seconds())
        .map_err(map_store)?
        .ok_or_else(|| failure("FORBIDDEN", "local demo session is expired"))?;
    state
        .actors
        .get(&actor_id)
        .cloned()
        .ok_or_else(|| failure("FORBIDDEN", "session actor is unavailable"))
}

fn idempotency_key(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| failure("INVALID_REQUEST", "Idempotency-Key is required"))
}

fn if_match(headers: &HeaderMap) -> Result<u64, ApiError> {
    let raw = headers
        .get("if-match")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| failure("INVALID_REQUEST", "If-Match event revision is required"))?;
    raw.trim_matches('"')
        .parse::<u64>()
        .ok()
        .filter(|revision| *revision > 0)
        .ok_or_else(|| {
            failure(
                "INVALID_REQUEST",
                "If-Match must be a positive event revision",
            )
        })
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(1)
}
fn sha256_hex(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn failure(code: &'static str, message: &str) -> ApiError {
    ApiError {
        error: ApiErrorBody {
            code,
            message: message.to_string(),
            retryable: false,
        },
    }
}
fn map_store(error: StoreError) -> ApiError {
    match error {
        StoreError::Domain(error) => map_domain(error),
        StoreError::Sql(_) => failure(
            "STORE_UNAVAILABLE",
            "local SQLite command could not be completed",
        ),
        StoreError::Json(_) | StoreError::InvalidCommandPayload => {
            failure("STORE_CORRUPT", "stored local command data is invalid")
        }
    }
}

fn map_domain(error: SlotForgeError) -> ApiError {
    let message = error.to_string();
    match error {
        SlotForgeError::UnknownEvent(_) | SlotForgeError::UnknownReservation(_) => {
            failure("NOT_FOUND", &message)
        }
        SlotForgeError::Forbidden { .. } | SlotForgeError::NotOwner { .. } => {
            failure("FORBIDDEN", &message)
        }
        SlotForgeError::IdempotencyConflict(_) => failure("IDEMPOTENCY_CONFLICT", &message),
        SlotForgeError::RevisionConflict { .. } => failure("REVISION_CONFLICT", &message),
        _ => failure("INVALID_COMMAND", &message),
    }
}
