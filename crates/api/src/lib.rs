//! Control-plane HTTP API.
//!
//! Exposes engine state (positions, inventory, orders, market state, risk) and
//! control endpoints (start / stop / reset / kill-switch) over HTTP. Reads
//! come from the shared [`EngineState`]; writes publish [`ControlEvent`]s onto
//! the control topic so the engine reacts to them asynchronously.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::ORIGIN;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lq_core::bus::{EventBus, PublishResult};
use lq_core::event::ControlEvent;
use lq_core::models::{Inventory, MarketState, Order, Position};
use lq_core::state::{EngineState, RiskStatus};
use serde::Serialize;

/// Everything a handler needs. Cheap to clone (Arcs + interior mutability).
#[derive(Clone)]
pub struct ApiState {
    pub state: EngineState,
    pub bus: Arc<EventBus>,
    /// Optional bearer token. When set, all API routes (except `/healthz`)
    /// require `Authorization: Bearer <token>`.
    pub token: Option<String>,
}

impl ApiState {
    pub fn new(state: EngineState, bus: Arc<EventBus>) -> Self {
        Self {
            state,
            bus,
            token: None,
        }
    }

    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
    }
}

/// Build the full control-plane router.
pub fn build_router(state: ApiState) -> Router {
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/state", get(state_summary))
        .route("/api/v1/positions", get(list_positions))
        .route("/api/v1/inventory", get(list_inventory))
        .route("/api/v1/orders", get(list_orders))
        .route("/api/v1/market-state", get(list_market_state))
        .route("/api/v1/risk", get(risk_status))
        .route("/api/v1/control/start", post(publish_start))
        .route("/api/v1/control/stop", post(publish_stop))
        .route("/api/v1/control/reset", post(publish_reset))
        .route("/api/v1/control/kill", post(publish_kill))
        .layer(axum::middleware::from_fn(cors));

    let router = if state.token.is_some() {
        router
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                auth,
            ))
            .layer(axum::middleware::from_fn(cors))
    } else {
        router.layer(axum::middleware::from_fn(cors))
    };

    router.with_state(state)
}

/// Reject requests without a valid `Authorization: Bearer <token>` header when
/// the API is configured with a token. `/healthz` is excluded so liveness
/// probes work without credentials.
async fn auth(
    State(api): State<ApiState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    // Liveness probes must work without credentials.
    if request.uri().path() == "/healthz" {
        return next.run(request).await;
    }
    // CORS preflight requests never carry credentials; let the CORS layer
    // answer them.
    if request.method() == Method::OPTIONS {
        return next.run(request).await;
    }
    let expected = api.token.as_deref().unwrap_or_default();
    let header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let valid = header
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|token| token == expected)
        .unwrap_or(false);
    if valid {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response()
    }
}

/// Permissive CORS for the web dashboard. The control plane is intentionally
/// unauthenticated (it is a local/paper tool); keep it bound to a private
/// interface in production and front it with auth if exposed.
async fn cors(request: axum::extract::Request, next: Next) -> Response {
    let method = request.method().clone();
    let origin = request.headers().get(ORIGIN).cloned();
    let mut response = next.run(request).await;

    if let Some(origin) = origin {
        let headers = response.headers_mut();
        headers.insert(
            "access-control-allow-origin",
            HeaderValue::from_str(origin.to_str().unwrap_or("*")).unwrap_or(HeaderValue::from_static("*")),
        );
        headers.insert("access-control-allow-methods", HeaderValue::from_static("GET, POST, OPTIONS"));
        headers.insert("access-control-allow-headers", HeaderValue::from_static("content-type, authorization"));
        headers.insert("access-control-max-age", HeaderValue::from_static("600"));
        if method == Method::OPTIONS {
            headers.insert("access-control-allow-credentials", HeaderValue::from_static("true"));
            *response.status_mut() = StatusCode::NO_CONTENT;
        }
    }
    response
}

/// One-shot aggregate view of engine state.
#[derive(Debug, Serialize)]
pub struct StateSummary {
    pub positions: Vec<Position>,
    pub inventory: Vec<Inventory>,
    pub orders: Vec<Order>,
    pub market_state: Vec<MarketState>,
    pub risk: RiskStatus,
    pub strategy_running: bool,
    pub uptime_ms: u64,
}

async fn healthz() -> &'static str {
    "ok"
}

async fn state_summary(State(api): State<ApiState>) -> Json<StateSummary> {
    let state = &api.state;
    let mut positions: Vec<_> = state
        .positions
        .iter()
        .map(|e| e.value().clone())
        .collect();
    positions.sort_by(|a, b| (a.venue.to_string(), a.symbol.as_str()).cmp(&(b.venue.to_string(), b.symbol.as_str())));

    let mut inventory: Vec<_> = state
        .inventory
        .iter()
        .map(|e| e.value().clone())
        .collect();
    inventory.sort_by(|a, b| a.symbol.as_str().cmp(b.symbol.as_str()));

    let mut orders: Vec<_> = state.orders.iter().map(|e| e.value().clone()).collect();
    orders.sort_by_key(|o| o.created_at);

    let mut market_state: Vec<_> = state
        .market_state
        .iter()
        .map(|e| e.value().clone())
        .collect();
    market_state.sort_by(|a, b| (a.venue.to_string(), a.symbol.as_str()).cmp(&(b.venue.to_string(), b.symbol.as_str())));

    Json(StateSummary {
        positions,
        inventory,
        orders,
        market_state,
        risk: state.risk_snapshot(),
        strategy_running: state.is_strategy_running(),
        uptime_ms: lq_types::TimestampMs::now().as_u64().saturating_sub(state.started_at.as_u64()),
    })
}

async fn list_positions(State(api): State<ApiState>) -> Json<Vec<Position>> {
    let mut positions: Vec<_> = api
        .state
        .positions
        .iter()
        .map(|e| e.value().clone())
        .collect();
    positions.sort_by(|a, b| (a.venue.to_string(), a.symbol.as_str()).cmp(&(b.venue.to_string(), b.symbol.as_str())));
    Json(positions)
}

async fn list_inventory(State(api): State<ApiState>) -> Json<Vec<Inventory>> {
    let mut inventory: Vec<_> = api
        .state
        .inventory
        .iter()
        .map(|e| e.value().clone())
        .collect();
    inventory.sort_by(|a, b| a.symbol.as_str().cmp(b.symbol.as_str()));
    Json(inventory)
}

async fn list_orders(State(api): State<ApiState>) -> Json<Vec<Order>> {
    let mut orders: Vec<_> = api.state.orders.iter().map(|e| e.value().clone()).collect();
    orders.sort_by_key(|o| o.created_at);
    Json(orders)
}

async fn list_market_state(State(api): State<ApiState>) -> Json<Vec<MarketState>> {
    let mut market_state: Vec<_> = api
        .state
        .market_state
        .iter()
        .map(|e| e.value().clone())
        .collect();
    market_state.sort_by(|a, b| (a.venue.to_string(), a.symbol.as_str()).cmp(&(b.venue.to_string(), b.symbol.as_str())));
    Json(market_state)
}

async fn risk_status(State(api): State<ApiState>) -> Json<RiskStatus> {
    Json(api.state.risk_snapshot())
}

async fn publish_start(State(api): State<ApiState>) -> ControlResponse {
    publish_control(&api.bus, ControlEvent::Start).await
}

async fn publish_stop(State(api): State<ApiState>) -> ControlResponse {
    publish_control(&api.bus, ControlEvent::Stop).await
}

async fn publish_reset(State(api): State<ApiState>) -> ControlResponse {
    publish_control(&api.bus, ControlEvent::Reset).await
}

#[derive(serde::Deserialize)]
pub struct KillBody {
    pub reason: String,
}

async fn publish_kill(State(api): State<ApiState>, Json(body): Json<KillBody>) -> ControlResponse {
    publish_control(&api.bus, ControlEvent::KillSwitch { reason: body.reason }).await
}

async fn publish_control(bus: &EventBus, event: ControlEvent) -> ControlResponse {
    match bus.control().publish_blocking(event).await {
        PublishResult::Published => ControlResponse {
            accepted: true,
            message: "published".into(),
        },
        PublishResult::Backpressure => ControlResponse {
            accepted: false,
            message: "control queue full; retry".into(),
        },
        PublishResult::Dropped => ControlResponse {
            accepted: false,
            message: "dropped".into(),
        },
        PublishResult::NoSubscribers => ControlResponse {
            accepted: false,
            message: "engine not listening".into(),
        },
    }
}

#[derive(Debug, Serialize)]
pub struct ControlResponse {
    pub accepted: bool,
    pub message: String,
}

impl IntoResponse for ControlResponse {
    fn into_response(self) -> Response {
        let status = if self.accepted {
            StatusCode::ACCEPTED
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };
        (status, Json(self)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode as HttpStatus};
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_ok() {
        let bus = Arc::new(EventBus::new());
        let api = ApiState::new(EngineState::new(), bus);
        let app = build_router(api);
        let res = app
            .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn state_endpoint_returns_json() {
        let bus = Arc::new(EventBus::new());
        let engine = EngineState::new();
        engine.set_strategy_running(true);
        let api = ApiState::new(engine, bus);
        let app = build_router(api);
        let res = app
            .oneshot(Request::builder().uri("/api/v1/state").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("strategy_running"));
        assert!(text.contains("true"));
    }

    #[tokio::test]
    async fn kill_switch_publishes() {
        let bus = Arc::new(EventBus::new());
        let mut sub = bus.control().subscribe();
        let api = ApiState::new(EngineState::new(), Arc::clone(&bus));
        let app = build_router(api);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/control/kill")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"reason":"test halt"}"#.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::ACCEPTED);
        let received = sub.recv().await;
        assert!(matches!(received, Some(ControlEvent::KillSwitch { reason }) if reason == "test halt"));
    }

    #[tokio::test]
    async fn auth_requires_bearer_token() {
        let bus = Arc::new(EventBus::new());
        let api = ApiState::new(EngineState::new(), bus).with_token(Some("s3cret".into()));
        let app = build_router(api);

        // No token -> 401 on a protected route.
        let res = app
            .clone()
            .oneshot(Request::builder().uri("/api/v1/state").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::UNAUTHORIZED);

        // Wrong token -> 401.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/state")
                    .header("authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::UNAUTHORIZED);

        // Correct token -> 200.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/state")
                    .header("authorization", "Bearer s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);

        // Healthz stays open.
        let res = app
            .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
    }
}