//! Control-plane HTTP API.
//!
//! Exposes engine state (positions, inventory, orders, market state, risk) and
//! control endpoints (start / stop / reset / kill-switch) over HTTP. Reads
//! come from the shared [`EngineState`]; writes publish [`ControlEvent`]s onto
//! the control topic so the engine reacts to them asynchronously.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ORIGIN};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lq_clob::ClobState;
use lq_core::bus::{EventBus, PublishResult};
use lq_core::event::ControlEvent;
use lq_core::models::{Inventory, MarketState, Order, Position};
use lq_core::state::{EngineState, RiskStatus};
use lq_orderbook::BookStore;
use lq_sequencer::{ApplyOutput, EntryPayload, LogEntry, MarketId, StateMachine, Wal};
use lq_types::{Price, Qty, Side, TimeInForce};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Everything a handler needs. Cheap to clone (Arcs + interior mutability).
#[derive(Clone)]
pub struct ApiState {
    pub state: EngineState,
    pub bus: Arc<EventBus>,
    /// Optional bearer token. When set, all API routes (except `/healthz`)
    /// require `Authorization: Bearer <token>`.
    pub token: Option<String>,
    /// Live market-data books (engine order books, fed by the feeds).
    pub books: Option<Arc<BookStore>>,
    /// Path to the sequencer WAL backing `GET /api/v1/log`.
    pub wal: Option<PathBuf>,
    /// Parsed index and replay over that WAL, shared by every log request so
    /// the file is read and folded incrementally instead of per call.
    log: Option<Arc<Mutex<LogCache>>>,
    /// Directory with the built dashboard served next to the API.
    pub web: Option<PathBuf>,
}

impl ApiState {
    pub fn new(state: EngineState, bus: Arc<EventBus>) -> Self {
        Self {
            state,
            bus,
            token: None,
            books: None,
            wal: None,
            log: None,
            web: None,
        }
    }

    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
    }

    /// Serve `GET /api/v1/book` from this store of live books.
    pub fn with_books(mut self, books: Arc<BookStore>) -> Self {
        self.books = Some(books);
        self
    }

    /// Serve `GET /api/v1/log` from the WAL at `path`.
    pub fn with_wal(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        self.wal = Some(path.clone());
        self.log = Some(Arc::new(Mutex::new(LogCache::new(path))));
        self
    }

    /// Serve the built dashboard from `dir` (usually `web/dist`): the router's
    /// fallback hands back assets and falls through to `index.html` for SPA
    /// routes, so the UI and the control plane share one origin.
    pub fn with_web(mut self, dir: impl Into<PathBuf>) -> Self {
        self.web = Some(dir.into());
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
        .route("/api/v1/book", get(book_levels))
        .route("/api/v1/log", get(log_entries))
        .route("/api/v1/control/start", post(publish_start))
        .route("/api/v1/control/stop", post(publish_stop))
        .route("/api/v1/control/reset", post(publish_reset))
        .route("/api/v1/control/kill", post(publish_kill))
        .fallback(spa)
        .layer(axum::middleware::from_fn(cors));

    let router = if state.token.is_some() {
        router
            .layer(axum::middleware::from_fn_with_state(state.clone(), auth))
            .layer(axum::middleware::from_fn(cors))
    } else {
        router.layer(axum::middleware::from_fn(cors))
    };

    router.with_state(state)
}

/// Reject requests without a valid `Authorization: Bearer <token>` header when
/// the API is configured with a token. Only the `/api` surface is protected:
/// liveness probes (`/healthz`) and the dashboard's static assets must load
/// without credentials.
async fn auth(
    State(api): State<ApiState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    // Non-API paths (dashboard assets, /healthz) stay public.
    if !request.uri().path().starts_with("/api/") {
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
            HeaderValue::from_str(origin.to_str().unwrap_or("*"))
                .unwrap_or(HeaderValue::from_static("*")),
        );
        headers.insert(
            "access-control-allow-methods",
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
        headers.insert(
            "access-control-allow-headers",
            HeaderValue::from_static("content-type, authorization"),
        );
        headers.insert("access-control-max-age", HeaderValue::from_static("600"));
        if method == Method::OPTIONS {
            headers.insert(
                "access-control-allow-credentials",
                HeaderValue::from_static("true"),
            );
            *response.status_mut() = StatusCode::NO_CONTENT;
        }
    }
    response
}

/// JSON 404 used for unknown API routes and missing static assets.
fn not_found(msg: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

/// Content type for a static asset, by extension.
fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// `GET /*` — the built dashboard, served from `ApiState::web` so the UI and
/// the control plane share one origin (no nginx/vite proxy in between).
///
/// Assets are served verbatim; anything that is not a file (an SPA route like
/// `/orders/123`) falls through to `index.html`. Requests under `/api/` never
/// reach here as files: unknown API routes stay JSON 404s.
async fn spa(State(api): State<ApiState>, request: axum::extract::Request) -> Response {
    let path = request.uri().path().to_string();
    if path.starts_with("/api/") || path == "/healthz" {
        return not_found("no such route");
    }
    let Some(root) = api.web.clone() else {
        return not_found("dashboard is not configured");
    };

    let rel = path.trim_start_matches('/');
    if rel.split('/').any(|seg| seg == "..") {
        return not_found("bad path");
    }

    let candidate = root.join(rel);
    let file = if rel.is_empty() || candidate.is_dir() {
        root.join("index.html")
    } else if candidate.is_file() {
        candidate
    } else if Path::new(rel).extension().is_some() {
        // A missing asset must 404, not return HTML.
        return not_found("no such asset");
    } else {
        root.join("index.html")
    };

    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let immutable = file.starts_with(root.join("assets"));
            let cache = if immutable {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            (
                [(CONTENT_TYPE, mime_for(&file)), (CACHE_CONTROL, cache)],
                bytes,
            )
                .into_response()
        }
        Err(_) => not_found("no such asset"),
    }
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
    let mut positions: Vec<_> = state.positions.iter().map(|e| e.value().clone()).collect();
    positions.sort_by(|a, b| {
        (a.venue.to_string(), a.symbol.as_str()).cmp(&(b.venue.to_string(), b.symbol.as_str()))
    });

    let mut inventory: Vec<_> = state.inventory.iter().map(|e| e.value().clone()).collect();
    inventory.sort_by(|a, b| a.symbol.as_str().cmp(b.symbol.as_str()));

    let mut orders: Vec<_> = state.orders.iter().map(|e| e.value().clone()).collect();
    orders.sort_by_key(|o| o.created_at);

    let mut market_state: Vec<_> = state
        .market_state
        .iter()
        .map(|e| e.value().clone())
        .collect();
    market_state.sort_by(|a, b| {
        (a.venue.to_string(), a.symbol.as_str()).cmp(&(b.venue.to_string(), b.symbol.as_str()))
    });

    Json(StateSummary {
        positions,
        inventory,
        orders,
        market_state,
        risk: state.risk_snapshot(),
        strategy_running: state.is_strategy_running(),
        uptime_ms: lq_types::TimestampMs::now()
            .as_u64()
            .saturating_sub(state.started_at.as_u64()),
    })
}

async fn list_positions(State(api): State<ApiState>) -> Json<Vec<Position>> {
    let mut positions: Vec<_> = api
        .state
        .positions
        .iter()
        .map(|e| e.value().clone())
        .collect();
    positions.sort_by(|a, b| {
        (a.venue.to_string(), a.symbol.as_str()).cmp(&(b.venue.to_string(), b.symbol.as_str()))
    });
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
    market_state.sort_by(|a, b| {
        (a.venue.to_string(), a.symbol.as_str()).cmp(&(b.venue.to_string(), b.symbol.as_str()))
    });
    Json(market_state)
}

async fn risk_status(State(api): State<ApiState>) -> Json<RiskStatus> {
    Json(api.state.risk_snapshot())
}

/// One aggregated price level (string decimals, like every other endpoint).
#[derive(Debug, Serialize)]
pub struct BookLevel {
    pub price: Price,
    pub qty: Qty,
}

/// Aggregated top-of-book for one market: `seq` is the book's venue sequence
/// number, levels are best-first (bids descending, asks ascending).
#[derive(Debug, Serialize)]
pub struct BookResponse {
    pub seq: u64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

#[derive(Debug, Deserialize)]
pub struct BookQuery {
    /// Symbol to serve (e.g. `BTC-USDT-PERP`). Omit for the first book.
    pub symbol: Option<String>,
    /// Venue to serve when several venues trade the same symbol. Omit for
    /// the first match in (venue, symbol) order.
    pub venue: Option<String>,
    /// Levels per side (default 20, clamped to 1..=500).
    pub depth: Option<usize>,
}

/// `GET /api/v1/book?symbol=&depth=` — aggregated bid/ask levels from the
/// live engine books. A missing/unregistered symbol yields empty levels
/// (200), so polling clients do not need to special-case it.
async fn book_levels(
    State(api): State<ApiState>,
    query: Result<Query<BookQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<BookResponse>, ApiError> {
    let q = query.map_err(|e| ApiError::BadRequest(e.to_string()))?.0;
    let depth = q.depth.unwrap_or(20).clamp(1, 500);
    let empty = BookResponse {
        seq: 0,
        bids: Vec::new(),
        asks: Vec::new(),
    };
    let Some(store) = api.books.as_deref() else {
        return Ok(Json(empty));
    };
    // `entries()` is sorted by (venue, symbol): the first match is stable.
    let Some((venue, symbol)) = store.entries().into_iter().find(|(v, s)| {
        q.symbol.as_deref().is_none_or(|sym| s.as_str() == sym)
            && q.venue.as_deref().is_none_or(|ev| v.as_str() == ev)
    }) else {
        return Ok(Json(empty));
    };
    let Some(book) = store.book(venue, &symbol) else {
        return Ok(Json(empty));
    };
    let snap = book.snapshot(depth);
    Ok(Json(BookResponse {
        seq: snap.sequence,
        bids: snap
            .bids
            .into_iter()
            .map(|l| BookLevel {
                price: l.price,
                qty: l.qty,
            })
            .collect(),
        asks: snap
            .asks
            .into_iter()
            .map(|l| BookLevel {
                price: l.price,
                qty: l.qty,
            })
            .collect(),
    }))
}

/// One sequencer log entry flattened for the dashboard.
#[derive(Debug, Serialize)]
pub struct LogRow {
    pub seq: u64,
    pub kind: String,
    pub side: Option<Side>,
    pub price: Option<Price>,
    pub qty: Option<Qty>,
    pub tif: Option<TimeInForce>,
    pub id: Option<Uuid>,
    /// Order a `replace_order` cancels; null for every other kind. The row's
    /// `id` is the replacement, so a client replaying the log needs both to
    /// drop the old order instead of leaving it resting.
    pub old_id: Option<Uuid>,
    pub reject_reason: Option<String>,
}

/// One open order as of the snapshot sequence.
#[derive(Debug, Serialize)]
pub struct SnapshotOrder {
    pub order_id: Uuid,
    pub side: Side,
    pub price: Option<Price>,
    pub remaining: Qty,
    /// `true` while the order rests in a book level.
    pub resting: bool,
}

/// Entries plus the head sequence and the state of replaying the log.
///
/// `head` is the last sequence in the WAL; `replayed` is how far the shared
/// replay has reached. They match as soon as the replay has caught up, which
/// is immediately for a small WAL and after a background catch-up for a large
/// one (folding a long log costs O(entries x orders), so it never runs inside
/// a request). `state_hash`, `orders` and `position` all describe the state
/// after the first `replayed` entries — the same prefix always hashes the
/// same way — and the dashboard rebuilds any earlier sequence from the
/// entries themselves.
#[derive(Debug, Serialize)]
pub struct LogResponse {
    /// Page selected by `from`/`limit`.
    pub entries: Vec<LogRow>,
    /// Last sequence in the WAL.
    pub head: u64,
    /// Sequence the state fields below reflect.
    pub replayed: u64,
    /// State hash of the replayed prefix, present only when `hash=1` was
    /// requested (it costs a walk over every order the state holds).
    pub state_hash: Option<String>,
    /// Open orders as of `replayed`.
    pub orders: Vec<SnapshotOrder>,
    /// Net base quantity across markets as of `replayed`.
    pub position: Qty,
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    /// First `global_seq` to return (inclusive). Defaults to the window of
    /// `limit` entries ending at the head.
    pub from: Option<u64>,
    /// Maximum entries to return (default 500, clamped to 1..=5000).
    pub limit: Option<usize>,
    /// Set (`hash=1`) to include `state_hash`. Hashing walks every order the
    /// state holds, so a poll does not pay for it.
    pub hash: Option<String>,
}

/// Work (entries x orders the state holds) a request may spend folding the log
/// before the rest goes to the background catch-up: a fraction of the sweep
/// `ClobState::apply` runs per entry, whatever the book happens to hold.
const INLINE_WORK: u64 = 5_000_000;
/// Target work per background chunk, in the same units. Small enough that a
/// request waiting on the mutex stays in the tens of milliseconds.
const CHUNK_WORK: u64 = 1_000_000;
/// Entries read and applied in one `replay_chunk` call, whatever the budget
/// says: with an almost empty state the budget would otherwise hand over the
/// whole file, and the sweep only gets dearer as the fold progresses.
const MAX_SLICE: usize = 500;

/// Incremental view of the WAL behind `GET /api/v1/log`.
///
/// Two cursors over the same file: `parsed` indexes the byte offset of every
/// sequence, so a page is one seek plus `limit` record reads instead of a
/// full-file parse; `replay_offset` folds entries into `state` incrementally,
/// so a request only pays for what the head has gained since the last one.
///
/// A large WAL is replayed by a background blocking task in chunks, because
/// `ClobState::apply` walks every order it holds and a full fold of a long log
/// takes minutes — work that must never sit inside a request or on the async
/// runtime. The mutex is only ever taken from a blocking task.
pub struct LogCache {
    path: PathBuf,
    /// Bytes indexed so far.
    parsed: u64,
    /// Records indexed so far.
    parsed_count: usize,
    /// Byte offset of the record carrying `global_seq == seq`.
    offsets: HashMap<u64, u64>,
    /// Last sequence indexed.
    head: u64,
    /// Bytes folded into `state`.
    replay_offset: u64,
    /// Records folded into `state`.
    replayed_count: usize,
    /// Last sequence folded into `state`.
    replayed: u64,
    state: ClobState,
    rejects: BTreeMap<u64, String>,
    markets: BTreeSet<MarketId>,
    /// A background catch-up task is running.
    warming: bool,
}

impl LogCache {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            parsed: 0,
            parsed_count: 0,
            offsets: HashMap::new(),
            head: 0,
            replay_offset: 0,
            replayed_count: 0,
            replayed: 0,
            state: ClobState::new(),
            rejects: BTreeMap::new(),
            markets: BTreeSet::new(),
            warming: false,
        }
    }

    /// Index bytes appended since the last call. A file that shrank (rotated,
    /// truncated, replaced) starts the index over.
    fn refresh(&mut self) -> Result<(), ApiError> {
        let len = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if len < self.parsed {
            *self = Self::new(self.path.clone());
        }
        if len > self.parsed {
            let (records, end) = Wal::read_from(&self.path, self.parsed, None)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            self.parsed_count += records.len();
            for (offset, entry) in records {
                self.head = self.head.max(entry.global_seq);
                self.offsets.insert(entry.global_seq, offset);
            }
            self.parsed = end;
        }
        Ok(())
    }

    /// Indexed entries not yet folded into `state`.
    fn pending(&self) -> usize {
        self.parsed_count.saturating_sub(self.replayed_count)
    }

    /// Orders the state holds — the width of the sweep `apply` runs per entry,
    /// and therefore the unit replay budgets are measured in.
    fn orders_held(&self) -> u64 {
        self.state.orders().count().max(1) as u64
    }

    /// Fold entries until roughly `budget` (entries x orders) has been spent,
    /// or the log runs out; returns how many entries were folded.
    fn replay_work(&mut self, budget: u64) -> Result<usize, ApiError> {
        let mut spent = 0;
        let mut folded = 0;
        while spent < budget {
            let pending = self.pending();
            if pending == 0 {
                break;
            }
            let orders = self.orders_held();
            let slice = (((budget - spent) / orders) as usize)
                .clamp(1, MAX_SLICE)
                .min(pending);
            let n = self.replay_chunk(slice)?;
            if n == 0 {
                break;
            }
            spent += n as u64 * orders;
            folded += n;
        }
        Ok(folded)
    }

    /// Fold up to `budget` pending entries into `state`; returns how many.
    fn replay_chunk(&mut self, budget: usize) -> Result<usize, ApiError> {
        if self.replay_offset >= self.parsed {
            return Ok(0);
        }
        let (records, end) = Wal::read_from(&self.path, self.replay_offset, Some(budget))
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        if records.is_empty() {
            return Ok(0);
        }
        for (_, entry) in &records {
            self.markets.insert(entry.market.clone());
            let outputs = self.state.apply(entry).map_err(|e| {
                ApiError::Internal(format!(
                    "wal replay failed at seq {}: {e}",
                    entry.global_seq
                ))
            })?;
            for out in outputs {
                if let ApplyOutput::Rejected { reason, .. } = out {
                    self.rejects.insert(entry.global_seq, reason.to_string());
                }
            }
            self.replayed = self.replayed.max(entry.global_seq);
            self.replayed_count += 1;
        }
        self.replay_offset = end;
        Ok(records.len())
    }

    /// Read the page starting at `from` (empty when the WAL has no such
    /// sequence).
    fn page(&self, from: u64, limit: usize) -> Result<Vec<LogRow>, ApiError> {
        let Some(&start) = self.offsets.get(&from) else {
            return Ok(Vec::new());
        };
        let (records, _) = Wal::read_from(&self.path, start, Some(limit))
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let state = &self.state;
        let rejects = &self.rejects;
        Ok(records
            .into_iter()
            .map(|(_, entry)| LogRow {
                reject_reason: rejects.get(&entry.global_seq).cloned(),
                ..log_row(&entry, state)
            })
            .collect())
    }

    /// Background chunk size: keep each chunk near `CHUNK_WORK` entry x order
    /// walks so the lock comes back quickly between requests.
    fn chunk(&self) -> u64 {
        CHUNK_WORK
    }
}

/// Open orders and net position after the replayed prefix.
fn orders_and_position(sm: &ClobState, markets: &BTreeSet<MarketId>) -> (Vec<SnapshotOrder>, Qty) {
    let orders = sm
        .orders()
        .filter(|o| !o.status.is_terminal())
        .map(|o| SnapshotOrder {
            order_id: o.order_id,
            side: o.side,
            price: o.price,
            remaining: o.remaining(),
            resting: o.is_resting(),
        })
        .collect();
    let position = markets
        .iter()
        .fold(Qty::ZERO, |acc, m| acc + sm.net_position(m));
    (orders, position)
}

fn log_row(entry: &LogEntry, sm: &ClobState) -> LogRow {
    let (side, price, qty, tif, id, old_id) = match &entry.payload {
        EntryPayload::PlaceOrder(c) => (
            Some(c.side),
            c.price,
            Some(c.quantity),
            Some(c.time_in_force),
            Some(c.order_id),
            None,
        ),
        EntryPayload::ReplaceOrder { old_order_id, new } => (
            Some(new.side),
            new.price,
            Some(new.quantity),
            Some(new.time_in_force),
            Some(new.order_id),
            Some(*old_order_id),
        ),
        EntryPayload::CancelOrder { order_id } => (None, None, None, None, Some(*order_id), None),
        EntryPayload::Fill(f) => (
            // A fill names the resting order: take its side from the replayed
            // state so the tape can colour fills without the client guessing.
            // `None` while a large WAL is still catching up; the dashboard's
            // own fold knows the side from the place entry.
            sm.order(f.order_id).map(|o| o.side),
            Some(f.price),
            Some(f.quantity),
            None,
            Some(f.order_id),
            None,
        ),
        EntryPayload::MarketTick(t) => (None, Some(t.last), None, None, None, None),
        EntryPayload::Transfer { .. }
        | EntryPayload::Liquidate { .. }
        | EntryPayload::SettleFunding { .. }
        | EntryPayload::OraclePrice(_) => (None, None, None, None, None, None),
    };
    LogRow {
        seq: entry.global_seq,
        kind: entry.payload_kind().to_string(),
        side,
        price,
        qty,
        tif,
        id,
        old_id,
        reject_reason: None,
    }
}

/// `GET /api/v1/log?from=&limit=` — a page of sequencer WAL entries plus the
/// head sequence and the state hash of replaying the log through the CLOB
/// state machine. Indexing, paging and replay run in a blocking task against
/// one shared cache, so a poll costs only what the head has gained and a long
/// log can never stall the async runtime or the rest of the API. Replay is
/// read-only: the WAL is the sole source and no wall clock or randomness is
/// involved.
async fn log_entries(
    State(api): State<ApiState>,
    query: Result<Query<LogQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<LogResponse>, ApiError> {
    let q = query.map_err(|e| ApiError::BadRequest(e.to_string()))?.0;
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    let from = q.from;
    let want_hash = hash_flag(&q.hash);
    tokio::task::spawn_blocking(move || api.log_response(from, limit, want_hash))
        .await
        .map_err(|e| ApiError::Internal(format!("log task failed: {e}")))?
        .map(Json)
}

impl ApiState {
    /// Build one `/api/v1/log` response; always called from a blocking task.
    fn log_response(
        &self,
        from: Option<u64>,
        limit: usize,
        want_hash: bool,
    ) -> Result<LogResponse, ApiError> {
        let Some(cache) = self.log.as_ref() else {
            // No WAL configured: an empty log at the genesis hash.
            let sm = ClobState::new();
            let state_hash = want_hash.then(|| sm.state_hash().as_hex());
            let (orders, position) = orders_and_position(&sm, &BTreeSet::new());
            return Ok(LogResponse {
                entries: Vec::new(),
                head: 0,
                replayed: 0,
                state_hash,
                orders,
                position,
            });
        };

        let mut start_warmup = false;
        let snapshot = {
            let mut c = cache
                .lock()
                .map_err(|_| ApiError::Internal("log cache poisoned".into()))?;
            c.refresh()?;
            // Fold what has arrived since the last call inline, up to a
            // work budget rather than a record count (one entry costs the
            // whole book to sweep). Anything left over — a cold, long WAL —
            // goes to the background catch-up: the response is still useful
            // (entries, head) without the full fold.
            if !c.warming {
                c.replay_work(INLINE_WORK)?;
                if c.pending() > 0 {
                    c.warming = true;
                    start_warmup = true;
                }
            }

            let head = c.head;
            let from = from.unwrap_or_else(|| head.saturating_sub(limit as u64 - 1).max(1));
            let entries = c.page(from, limit)?;
            let (orders, position) = orders_and_position(&c.state, &c.markets);
            // Cloning is far cheaper than hashing: the hash walks (and
            // formats) every order the state holds, so it is computed outside
            // the mutex, and only when asked for.
            let snap = want_hash.then(|| c.state.clone());
            let replayed = c.replayed;
            (
                LogResponse {
                    entries,
                    head,
                    replayed,
                    state_hash: None,
                    orders,
                    position,
                },
                snap,
            )
        };
        let (mut response, snap) = snapshot;
        if let Some(sm) = snap {
            response.state_hash = Some(sm.state_hash().as_hex());
        }

        if start_warmup {
            // Outside the lock: fold the rest of the log in chunks, yielding
            // between them so requests keep being served while it runs.
            let cache = Arc::clone(cache);
            tokio::task::spawn_blocking(move || loop {
                let progress = {
                    let mut c = match cache.lock() {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let chunk = c.chunk();
                    matches!(c.replay_work(chunk), Ok(n) if n > 0)
                };
                if !progress {
                    if let Ok(mut c) = cache.lock() {
                        c.warming = false;
                    }
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            });
        }

        Ok(response)
    }
}

/// The log endpoint's `hash` query flag: present and `1`, `true`, `yes`, `on`
/// or empty. Absent means no hash.
fn hash_flag(v: &Option<String>) -> bool {
    v.as_deref()
        .is_some_and(|v| v.is_empty() || matches!(v, "1" | "true" | "yes" | "on"))
}

/// JSON error body matching the auth layer's `{"error": ...}` shape.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = serde_json::json!({ "error": self.to_string() });
        (status, Json(body)).into_response()
    }
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
    publish_control(
        &api.bus,
        ControlEvent::KillSwitch {
            reason: body.reason,
        },
    )
    .await
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
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
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
            .oneshot(
                Request::builder()
                    .uri("/api/v1/state")
                    .body(Body::empty())
                    .unwrap(),
            )
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
        assert!(
            matches!(received, Some(ControlEvent::KillSwitch { reason }) if reason == "test halt")
        );
    }

    #[tokio::test]
    async fn auth_requires_bearer_token() {
        let bus = Arc::new(EventBus::new());
        let api = ApiState::new(EngineState::new(), bus).with_token(Some("s3cret".into()));
        let app = build_router(api);

        // No token -> 401 on a protected route.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/state")
                    .body(Body::empty())
                    .unwrap(),
            )
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
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
    }

    fn btc() -> lq_types::Symbol {
        "BTC-USDT".parse().unwrap()
    }

    fn level(price: u64, qty: u64) -> lq_core::models::OrderBookLevel {
        lq_core::models::OrderBookLevel::new(price.into(), qty.into())
    }

    #[tokio::test]
    async fn book_endpoint_returns_levels() {
        use lq_core::models::OrderBookSnapshot;
        use lq_orderbook::BookStore;
        use lq_types::{Exchange, TimestampMs};

        let store = Arc::new(BookStore::new());
        store.apply_snapshot(&OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: btc(),
            sequence: 7,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
            bids: vec![level(64_000, 2), level(63_990, 5)],
            asks: vec![level(64_010, 3), level(64_020, 1)],
        });
        let api = ApiState::new(EngineState::new(), Arc::new(EventBus::new())).with_books(store);
        let app = build_router(api);

        // depth=1 caps each side; levels are best-first; decimals are strings.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/book?symbol=BTC-USDT&depth=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["seq"], 7);
        assert_eq!(v["bids"].as_array().unwrap().len(), 1);
        assert_eq!(v["asks"].as_array().unwrap().len(), 1);
        // Decimals are strings; the book rounds to tick, so parse rather
        // than asserting a literal representation.
        assert!(v["bids"][0]["price"].is_string());
        let px: Price = v["bids"][0]["price"].as_str().unwrap().parse().unwrap();
        assert_eq!(px, Price::from(64_000u64));
        let qty: Qty = v["bids"][0]["qty"].as_str().unwrap().parse().unwrap();
        assert_eq!(qty, Qty::from(2u64));
        let apx: Price = v["asks"][0]["price"].as_str().unwrap().parse().unwrap();
        assert_eq!(apx, Price::from(64_010u64));

        // Unknown symbol → empty levels, still 200.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/book?symbol=NOPE")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["seq"], 0);
        assert!(v["bids"].as_array().unwrap().is_empty());
        assert!(v["asks"].as_array().unwrap().is_empty());

        // Malformed query → 400 with a JSON error body.
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/book?depth=abc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::BAD_REQUEST);
    }

    fn append(
        wal: &mut Wal,
        seq: u64,
        market_seq: u64,
        market: &lq_sequencer::MarketId,
        payload: EntryPayload,
    ) {
        wal.append(&LogEntry {
            global_seq: seq,
            market_seq,
            market: market.clone(),
            ts_ms: seq,
            payload,
        })
        .unwrap();
    }

    #[tokio::test]
    async fn log_endpoint_reads_wal() {
        use lq_sequencer::{MarketId, PlaceOrderCmd};
        use lq_types::{Exchange, OrderType, Side, TimeInForce};
        use uuid::Uuid;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open(&path).unwrap();
        let market = MarketId::new(Exchange::Paper, btc());

        // 1: resting bid. 2: post-only ask that crosses it → rejected on
        // replay. 3: cancel the bid.
        let bid_id = Uuid::new_v4();
        append(
            &mut wal,
            1,
            1,
            &market,
            EntryPayload::PlaceOrder(PlaceOrderCmd {
                order_id: bid_id,
                client_order_id: "c1".into(),
                side: Side::Bid,
                order_type: OrderType::Limit,
                price: Some(100.into()),
                quantity: 1.into(),
                time_in_force: TimeInForce::Gtc,
                ..Default::default()
            }),
        );
        append(
            &mut wal,
            2,
            2,
            &market,
            EntryPayload::PlaceOrder(PlaceOrderCmd {
                order_id: Uuid::new_v4(),
                side: Side::Ask,
                order_type: OrderType::PostOnly,
                price: Some(99.into()),
                quantity: 1.into(),
                time_in_force: TimeInForce::Gtc,
                ..Default::default()
            }),
        );
        append(
            &mut wal,
            3,
            3,
            &market,
            EntryPayload::CancelOrder { order_id: bid_id },
        );
        drop(wal);

        let api = ApiState::new(EngineState::new(), Arc::new(EventBus::new())).with_wal(&path);
        let app = build_router(api);

        // Full read: entries, head, replay hash, reject reason.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/log")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["head"], 3);
        assert_eq!(v["entries"].as_array().unwrap().len(), 3);
        // Hashing every order is opt-in: a plain poll does not pay for it.
        assert!(v["state_hash"].is_null());
        assert_eq!(v["entries"][0]["seq"], 1);
        assert_eq!(v["entries"][0]["kind"], "place_order");
        assert_eq!(v["entries"][0]["side"], "bid");
        assert_eq!(v["entries"][0]["price"], "100");
        assert_eq!(v["entries"][0]["qty"], "1");
        assert_eq!(v["entries"][0]["tif"], "gtc");
        assert!(v["entries"][0]["reject_reason"].is_null());
        assert_eq!(v["entries"][1]["kind"], "place_order");
        assert_eq!(v["entries"][1]["reject_reason"], "post_only_cross");
        assert_eq!(v["entries"][2]["kind"], "cancel_order");
        assert_eq!(v["entries"][2]["seq"], 3);

        // `hash=1` computes the replay hash of the whole log.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/log?hash=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["state_hash"].as_str().unwrap().len(), 64);
        assert_eq!(v["entries"].as_array().unwrap().len(), 3);

        // from/limit paging.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/log?from=2&limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["head"], 3);
        assert_eq!(v["entries"].as_array().unwrap().len(), 1);
        assert_eq!(v["entries"][0]["seq"], 2);

        // Beyond the head: empty page, head still reported.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/log?from=99")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["head"], 3);
        assert!(v["entries"].as_array().unwrap().is_empty());

        // Malformed query → 400.
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/log?limit=abc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn log_endpoint_without_wal_is_empty() {
        let api = ApiState::new(EngineState::new(), Arc::new(EventBus::new()));
        let app = build_router(api);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/log?hash=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["head"], 0);
        assert_eq!(v["replayed"], 0);
        assert!(v["entries"].as_array().unwrap().is_empty());
        // Genesis hash of an empty replay — same value the WAL path would
        // return for an empty file.
        let genesis = ClobState::new().state_hash().as_hex();
        assert_eq!(v["state_hash"], genesis);
    }

    #[tokio::test]
    async fn log_endpoint_replays_prefix_and_pages() {
        use lq_sequencer::{MarketId, PlaceOrderCmd};
        use lq_types::{Exchange, OrderType, Side, TimeInForce};
        use uuid::Uuid;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open(&path).unwrap();
        let market = MarketId::new(Exchange::Paper, btc());

        // 1: resting bid. 2: resting ask. 3: cancel the bid.
        let bid_id = Uuid::new_v4();
        for (seq, side, price, id) in [
            (1u64, Side::Bid, 100u64, bid_id),
            (2, Side::Ask, 110, Uuid::new_v4()),
        ] {
            append(
                &mut wal,
                seq,
                seq,
                &market,
                EntryPayload::PlaceOrder(PlaceOrderCmd {
                    order_id: id,
                    client_order_id: format!("c{seq}"),
                    side,
                    order_type: OrderType::Limit,
                    price: Some(price.into()),
                    quantity: 1.into(),
                    time_in_force: TimeInForce::Gtc,
                    ..Default::default()
                }),
            );
        }
        append(
            &mut wal,
            3,
            3,
            &market,
            EntryPayload::CancelOrder { order_id: bid_id },
        );
        drop(wal);

        let api = ApiState::new(EngineState::new(), Arc::new(EventBus::new())).with_wal(&path);
        let app = build_router(api);

        // Reference replay of the same WAL from scratch: the endpoint's
        // `state_hash` must equal it once the replay has caught up.
        let entries = Wal::read_all(&path).unwrap();
        let mut sm = ClobState::new();
        for entry in &entries {
            sm.apply(entry).unwrap();
        }
        let full_hash = sm.state_hash().as_hex();

        // Default: window ends at the head, and the replay has caught up.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/log?hash=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["head"], 3);
        assert_eq!(v["replayed"], 3);
        assert_eq!(v["entries"].as_array().unwrap().len(), 3);
        assert_eq!(v["state_hash"], full_hash);
        // The bid was cancelled: only the ask is left, still resting.
        assert_eq!(v["orders"].as_array().unwrap().len(), 1);
        assert_eq!(v["orders"][0]["price"], "110");
        assert_eq!(v["orders"][0]["resting"], true);
        assert_eq!(v["position"], "0");
        // Snapshots are gone: any sequence is rebuilt from the entries.
        assert!(v.get("at").is_none());
        assert!(v.get("at_state_hash").is_none());

        // An explicit page starts at `from` and still reports the head.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/log?from=2&limit=1&hash=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["head"], 3);
        assert_eq!(v["replayed"], 3);
        assert_eq!(v["entries"].as_array().unwrap().len(), 1);
        assert_eq!(v["entries"][0]["seq"], 2);
        assert_eq!(v["state_hash"], full_hash);
        // `at` is no longer part of the contract: it is ignored if sent.
        assert!(v.get("at").is_none());

        // A page past the head: empty, head still reported.
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/log?from=3&limit=5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["head"], 3);
        assert_eq!(v["entries"].as_array().unwrap().len(), 1);
        assert_eq!(v["entries"][0]["seq"], 3);
    }

    /// A WAL larger than the inline fold budget is replayed in the
    /// background: the first response still pages entries, and the replay
    /// catches up to the head without blocking the request.
    #[tokio::test]
    async fn log_endpoint_warms_up_long_wal_in_background() {
        use lq_sequencer::{MarketId, PlaceOrderCmd};
        use lq_types::{Exchange, OrderType, Side, TimeInForce};
        use uuid::Uuid;

        // All resting bids: folding N of them costs ~N^2/2 order walks, so
        // this is comfortably past what a request will spend inline.
        const N: u64 = 12_000;
        const { assert!(N * N / 2 > INLINE_WORK) };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open(&path).unwrap();
        wal.set_sync_on_append(false);
        let market = MarketId::new(Exchange::Paper, btc());
        for seq in 1..=N {
            append(
                &mut wal,
                seq,
                seq,
                &market,
                EntryPayload::PlaceOrder(PlaceOrderCmd {
                    order_id: Uuid::new_v4(),
                    client_order_id: format!("c{seq}"),
                    side: Side::Bid,
                    order_type: OrderType::Limit,
                    price: Some((100 + seq).into()),
                    quantity: 1.into(),
                    time_in_force: TimeInForce::Gtc,
                    ..Default::default()
                }),
            );
        }
        drop(wal);

        // Reference replay from scratch (all bids, all resting).
        let entries = Wal::read_all(&path).unwrap();
        let mut sm = ClobState::new();
        for entry in &entries {
            sm.apply(entry).unwrap();
        }
        let full_hash = sm.state_hash().as_hex();
        assert_eq!(entries.len(), N as usize);

        let api = ApiState::new(EngineState::new(), Arc::new(EventBus::new())).with_wal(&path);
        let app = build_router(api);

        // First call: entries and head come back without waiting for the
        // whole log to be folded.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/log")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["head"], N);
        let rows = v["entries"].as_array().unwrap();
        assert_eq!(rows.len(), 500);
        assert_eq!(rows[499]["seq"], N);

        // The background catch-up reaches the head, and then the hash and the
        // snapshot match the from-scratch replay.
        let mut caught_up = false;
        for _ in 0..400 {
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/v1/log?limit=1")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["head"], N);
            if v["replayed"] == N {
                caught_up = true;
                break;
            }
            assert!(v["replayed"].as_u64().unwrap() <= N);
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(caught_up, "background replay never reached the head");

        // Once caught up, the opt-in hash matches a from-scratch replay, and
        // the snapshot shows every order still resting.
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/log?limit=1&hash=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["state_hash"], full_hash);
        assert_eq!(v["orders"].as_array().unwrap().len(), N as usize);
        assert_eq!(v["position"], "0");
    }

    #[tokio::test]
    async fn spa_serves_dashboard_and_keeps_api_404_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(
            dir.path().join("index.html"),
            "<div id=root>dashboard</div>",
        )
        .unwrap();
        std::fs::write(dir.path().join("assets/app.js"), "console.log(1)").unwrap();

        let api = ApiState::new(EngineState::new(), Arc::new(EventBus::new()))
            .with_token(Some("s3cret".into()))
            .with_web(dir.path());
        let app = build_router(api);

        // Assets are public: they load without the bearer token.
        let res = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("dashboard"));

        // Hashed asset: long-lived cache header.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/assets/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            res.headers().get("cache-control").unwrap(),
            "public, max-age=31536000, immutable"
        );

        // SPA route -> index.html; missing asset -> 404; unknown API -> JSON 404.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/orders/123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("dashboard"));

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/assets/missing.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::NOT_FOUND);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nope")
                    .header("authorization", "Bearer s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), HttpStatus::NOT_FOUND);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].is_string());

        // Traversal attempts never escape the web root.
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/../../etc/passwd")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(res.status(), HttpStatus::OK);
    }
}
