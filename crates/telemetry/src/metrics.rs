//! Prometheus metrics and the `/metrics` endpoint.
//!
//! The [`Metrics`] struct owns a [`Registry`] and exposes typed record methods
//! so the engine never touches metric primitives directly. A single
//! [`MetricsServer`] can then serve the whole registry over HTTP for scraping.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{header, Response, StatusCode};
use axum::routing::get;
use axum::Router;
use axum::body::Body;
use lq_core::bus::EventBus;
use lq_core::event::{ExecutionEvent, MarketEvent, MarketEventKind};
use lq_core::models::LatencyMeasurement;
use lq_core::state::EngineState;

use prometheus::core::Collector;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, Registry, TextEncoder,
};

/// Nanosecond histogram buckets covering sub-microsecond decode times through
/// multi-millisecond end-to-end round trips.
pub const LATENCY_BUCKETS: &[f64] = &[
    1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1_000.0, 2_500.0, 5_000.0, 10_000.0,
    50_000.0, 100_000.0, 500_000.0, 1_000_000.0,
];

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("metrics encode failed: {0}")]
    Encode(String),
}

/// Thread-safe handle to the process metric registry.
#[derive(Clone)]
pub struct Metrics {
    registry: Arc<Registry>,
    market_events: IntCounterVec,
    execution_events: IntCounterVec,
    fills_total: IntCounterVec,
    fees_total: prometheus::GaugeVec,
    latency: HistogramVec,
    open_orders: IntGaugeVec,
    net_position: prometheus::GaugeVec,
    inventory_qty: prometheus::GaugeVec,
    realized_pnl: prometheus::GaugeVec,
    halted: IntGaugeVec,
    strategy_running: IntGaugeVec,
    topic_published: IntCounterVec,
    topic_dropped: IntCounterVec,
    topic_no_subscribers: IntCounterVec,
    topic_subscribers: IntGaugeVec,
    /// Last-seen cumulative topic stats, so `observe_bus` reports deltas and
    /// the Prometheus counters do not grow quadratically across ticks.
    topic_last: Arc<Mutex<[(u64, u64, u64); 3]>>,
}

fn register(registry: &Registry, collector: Box<dyn Collector>) {
    if let Err(e) = registry.register(collector) {
        tracing::warn!(err = %e, "metric registration skipped");
    }
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Arc::new(Registry::new());

        let market_events = IntCounterVec::new(
            Opts::new("lq_market_events_total", "Market events received by kind."),
            &["kind"],
        )
        .expect("metric");
        let execution_events = IntCounterVec::new(
            Opts::new("lq_execution_events_total", "Execution events by kind."),
            &["kind"],
        )
        .expect("metric");
        let fills_total = IntCounterVec::new(
            Opts::new("lq_fills_total", "Fills by venue."),
            &["venue"],
        )
        .expect("metric");
        let fees_total = prometheus::GaugeVec::new(
            Opts::new("lq_fees_total", "Cumulative fees (rebates negative) by venue."),
            &["venue"],
        )
        .expect("metric");
        let latency = HistogramVec::new(
            HistogramOpts::new(
                "lq_latency_ns",
                "Pipeline stage latency in nanoseconds.",
            )
            .buckets(LATENCY_BUCKETS.to_vec()),
            &["stage"],
        )
        .expect("metric");
        let open_orders = IntGaugeVec::new(
            Opts::new("lq_open_orders", "Working orders by venue."),
            &["venue"],
        )
        .expect("metric");
        let net_position = prometheus::GaugeVec::new(
            Opts::new("lq_net_position", "Signed net position by venue and symbol."),
            &["venue", "symbol"],
        )
        .expect("metric");
        let inventory_qty = prometheus::GaugeVec::new(
            Opts::new("lq_inventory_qty", "Aggregated inventory by symbol."),
            &["symbol"],
        )
        .expect("metric");
        let realized_pnl = prometheus::GaugeVec::new(
            Opts::new("lq_realized_pnl", "Realized PnL by symbol."),
            &["symbol"],
        )
        .expect("metric");
        let halted = IntGaugeVec::new(Opts::new("lq_halted", "Kill switch state."), &[])
            .expect("metric");
        let strategy_running = IntGaugeVec::new(
            Opts::new("lq_strategy_running", "Strategy enabled."),
            &["strategy"],
        )
        .expect("metric");
        let topic_published = IntCounterVec::new(
            Opts::new("lq_topic_published_total", "Events published by topic."),
            &["topic"],
        )
        .expect("metric");
        let topic_dropped = IntCounterVec::new(
            Opts::new("lq_topic_dropped_total", "Events dropped by topic."),
            &["topic"],
        )
        .expect("metric");
        let topic_no_subscribers = IntCounterVec::new(
            Opts::new(
                "lq_topic_no_subscribers_total",
                "Events with no subscribers by topic.",
            ),
            &["topic"],
        )
        .expect("metric");
        let topic_subscribers = IntGaugeVec::new(
            Opts::new("lq_topic_subscribers", "Live subscribers by topic."),
            &["topic"],
        )
        .expect("metric");

        for c in [
            Box::new(market_events.clone()) as Box<dyn Collector>,
            Box::new(execution_events.clone()),
            Box::new(fills_total.clone()),
            Box::new(fees_total.clone()),
            Box::new(latency.clone()),
            Box::new(open_orders.clone()),
            Box::new(net_position.clone()),
            Box::new(inventory_qty.clone()),
            Box::new(realized_pnl.clone()),
            Box::new(halted.clone()),
            Box::new(strategy_running.clone()),
            Box::new(topic_published.clone()),
            Box::new(topic_dropped.clone()),
            Box::new(topic_no_subscribers.clone()),
            Box::new(topic_subscribers.clone()),
        ] {
            register(&registry, c);
        }

        Self {
            registry,
            market_events,
            execution_events,
            fills_total,
            fees_total,
            latency,
            open_orders,
            net_position,
            inventory_qty,
            realized_pnl,
            halted,
            strategy_running,
            topic_published,
            topic_dropped,
            topic_no_subscribers,
            topic_subscribers,
            topic_last: Arc::new(Mutex::new([(0, 0, 0); 3])),
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    fn kind_label(kind: MarketEventKind) -> &'static str {
        match kind {
            MarketEventKind::Snapshot => "snapshot",
            MarketEventKind::Delta => "delta",
            MarketEventKind::Trade => "trade",
            MarketEventKind::Tick => "tick",
            MarketEventKind::Status => "status",
        }
    }

    pub fn record_market_event(&self, event: &MarketEvent) {
        self.market_events
            .with_label_values(&[Self::kind_label(event.kind())])
            .inc();
    }

    pub fn record_execution(&self, event: &ExecutionEvent) {
        let kind = match event {
            ExecutionEvent::New { .. } => "new",
            ExecutionEvent::Acknowledged { .. } => "acknowledged",
            ExecutionEvent::Fill(_) => "fill",
            ExecutionEvent::CancelRequested { .. } => "cancel_requested",
            ExecutionEvent::Cancelled { .. } => "cancelled",
            ExecutionEvent::Rejected { .. } => "rejected",
            ExecutionEvent::Expired { .. } => "expired",
            ExecutionEvent::Trade { .. } => "trade",
        };
        self.execution_events.with_label_values(&[kind]).inc();

        if let ExecutionEvent::Fill(fill) = event {
            let venue = fill.venue.to_string();
            self.fills_total.with_label_values(&[&venue]).inc();
            self.fees_total
                .with_label_values(&[&venue])
                .add(fill.fee.as_f64());
        }
    }

    /// Record a stage latency in nanoseconds.
    pub fn record_latency(&self, measurement: &LatencyMeasurement) {
        let stage = match measurement.stage {
            lq_core::models::LatencyStage::ExchangeReceive => "exchange_receive",
            lq_core::models::LatencyStage::Decode => "decode",
            lq_core::models::LatencyStage::OrderBookUpdate => "order_book_update",
            lq_core::models::LatencyStage::MarketState => "market_state",
            lq_core::models::LatencyStage::Strategy => "strategy",
            lq_core::models::LatencyStage::Risk => "risk",
            lq_core::models::LatencyStage::ExecutionSubmit => "execution_submit",
            lq_core::models::LatencyStage::ExecutionAck => "execution_ack",
            lq_core::models::LatencyStage::ExecutionFill => "execution_fill",
            lq_core::models::LatencyStage::EndToEnd => "end_to_end",
        };
        self.latency
            .with_label_values(&[stage])
            .observe(measurement.nanos as f64);
    }

    /// Refresh gauges from the current [`EngineState`].
    pub fn observe_state(&self, state: &EngineState) {
        // Positions and inventory.
        for pos in state.positions.iter() {
            let (venue, symbol) = pos.key();
            self.net_position
                .with_label_values(&[&venue.to_string(), symbol.as_str()])
                .set(pos.value().net_qty.as_f64());
        }
        for inv in state.inventory.iter() {
            let symbol = inv.key();
            self.inventory_qty
                .with_label_values(&[symbol.as_str()])
                .set(inv.value().net_qty.as_f64());
            self.realized_pnl
                .with_label_values(&[symbol.as_str()])
                .set(inv.value().realized_pnl.as_f64());
        }

        // Open orders grouped by venue.
        let mut open: Vec<(String, i64)> = Vec::new();
        for order in state.orders.iter() {
            if order.value().status.is_terminal() {
                continue;
            }
            let venue = order.value().venue.to_string();
            match open.iter_mut().find(|(v, _)| **v == venue) {
                Some((_, n)) => *n += 1,
                None => open.push((venue, 1)),
            }
        }
        for (venue, n) in open {
            self.open_orders.with_label_values(&[&venue]).set(n);
        }

        let risk = state.risk_snapshot();
        self.halted.with_label_values(&[]).set(i64::from(risk.halted));
        self.strategy_running
            .with_label_values(&["market_making"])
            .set(i64::from(state.is_strategy_running()));
    }

    /// Record topic statistics from the bus.
    pub fn observe_bus(&self, bus: &EventBus) {
        let mut last = self.topic_last.lock().unwrap();
        for (i, (name, stats)) in [
            ("market", bus.market_stats()),
            ("execution", bus.execution().stats()),
            ("control", bus.control().stats()),
        ]
        .iter()
        .enumerate()
        {
            let prev = &mut last[i];
            let published_delta = stats.published.saturating_sub(prev.0);
            let dropped_delta = stats.dropped.saturating_sub(prev.1);
            let no_sub_delta = stats.no_subscribers.saturating_sub(prev.2);
            *prev = (stats.published, stats.dropped, stats.no_subscribers);
            self.topic_published
                .with_label_values(&[name])
                .inc_by(published_delta);
            self.topic_dropped
                .with_label_values(&[name])
                .inc_by(dropped_delta);
            self.topic_no_subscribers
                .with_label_values(&[name])
                .inc_by(no_sub_delta);
            self.topic_subscribers
                .with_label_values(&[name])
                .set(stats.subscribers as i64);
        }
    }

    /// Render the full registry in Prometheus text exposition format.
    pub fn encode(&self) -> Result<String, TelemetryError> {
        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();
        encoder
            .encode(&self.registry.gather(), &mut buffer)
            .map_err(|e| TelemetryError::Encode(e.to_string()))?;
        String::from_utf8(buffer).map_err(|e| TelemetryError::Encode(e.to_string()))
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Serves the metrics registry over HTTP for Prometheus scraping.
pub struct MetricsServer {
    metrics: Arc<Metrics>,
}

impl MetricsServer {
    pub fn new(metrics: Arc<Metrics>) -> Self {
        Self { metrics }
    }

    pub async fn serve(self, bind: &str) -> anyhow::Result<()> {
        let app = Router::new()
            .route("/metrics", get(metrics_handler))
            .route("/healthz", get(healthz))
            .with_state(self.metrics);
        let listener = tokio::net::TcpListener::bind(bind).await?;
        tracing::info!(bind, "metrics server listening");
        axum::serve(listener, app).await?;
        Ok(())
    }
}

async fn metrics_handler(State(metrics): State<Arc<Metrics>>) -> Response<Body> {
    match metrics.encode() {
        Ok(body) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/plain; version=0.0.4")
            .body(Body::from(body))
            .expect("valid response"),
        Err(e) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from(format!("{e}")))
            .expect("valid response"),
    }
}

async fn healthz() -> &'static str {
    "ok"
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::bus::EventBus;
    use lq_core::event::FeedStatus;
    use lq_core::models::{FillEvent, OrderBookSnapshot, OrderBookLevel};
    use lq_types::{Exchange, Side, Symbol, TimestampMs};
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    #[test]
    fn records_market_events_by_kind() {
        let m = Metrics::new();
        m.record_market_event(&MarketEvent::Delta(sample_delta(1)));
        m.record_market_event(&MarketEvent::Snapshot(sample_snapshot()));
        m.record_market_event(&MarketEvent::Status {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            status: FeedStatus::Healthy,
            ts: TimestampMs::now(),
        });
        let text = m.encode().unwrap();
        assert!(text.contains("lq_market_events_total"));
    }

    #[test]
    fn records_fills_and_fees() {
        let m = Metrics::new();
        let fill = FillEvent {
            execution_id: Uuid::new_v4(),
            order_id: Uuid::new_v4(),
            client_order_id: "c1".into(),
            venue: Exchange::Simulated,
            symbol: Symbol("BTC-USDT".into()),
            side: Side::Bid,
            price: dec!(100.0),
            qty: dec!(0.5),
            fee: dec!(-0.0125),
            fee_currency: "USDT".into(),
            exchange_ts: TimestampMs::now(),
            event_ts: TimestampMs::now(),
        };
        m.record_execution(&ExecutionEvent::Fill(fill));
        m.record_execution(&ExecutionEvent::Rejected {
            order_id: Uuid::new_v4(),
            venue: Exchange::Paper,
            reason: "nope".into(),
            ts: TimestampMs::now(),
        });
        let text = m.encode().unwrap();
        assert!(text.contains("lq_fills_total"));
        assert!(text.contains("lq_fees_total"));
        assert!(text.contains("lq_execution_events_total"));
    }

    #[test]
    fn latency_histogram_records() {
        let m = Metrics::new();
        let meas = LatencyMeasurement {
            stage: lq_core::models::LatencyStage::Decode,
            nanos: 42_000,
            event_ts: TimestampMs::now(),
        };
        m.record_latency(&meas);
        let text = m.encode().unwrap();
        assert!(text.contains("lq_latency_ns_bucket"));
    }

    #[tokio::test]
    async fn observes_engine_state() {
        let m = Metrics::new();
        let state = EngineState::new();
        state.set_strategy_running(true);
        m.observe_state(&state);
        m.observe_bus(&EventBus::new());
        let text = m.encode().unwrap();
        assert!(text.contains("lq_strategy_running"));
        assert!(text.contains("lq_topic_published_total"));
        assert!(text.contains("lq_halted"));
    }

    fn sample_delta(seq: u64) -> lq_core::models::OrderBookDelta {
        lq_core::models::OrderBookDelta {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: seq,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
            changes: vec![lq_core::models::LevelChange {
                side: Side::Bid,
                price: dec!(100),
                qty: dec!(1),
            }],
            clear: false,
        }
    }

    fn sample_snapshot() -> OrderBookSnapshot {
        OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: 1,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
            bids: vec![OrderBookLevel::new(dec!(99.8), dec!(10.0))],
            asks: vec![OrderBookLevel::new(dec!(100.2), dec!(10.0))],
        }
    }
}