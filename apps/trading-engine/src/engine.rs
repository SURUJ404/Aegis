//! Engine wiring: market data -> book -> analytics -> strategy -> risk ->
//! execution -> position tracking -> observability.
//!
//! The engine runs a single-threaded event loop over the market, execution and
//! control topics. Strategies are pure and owned by the loop, so their state
//! (quote timing, inventory skew) stays consistent without locks.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use lq_api::ApiState;
use lq_core::bus::EventBus;
use lq_core::config::{EngineConfig, Mode};
use lq_core::event::{ControlEvent, FeedStatus, MarketEvent};
use lq_core::models::{Order, StrategyDecision};
use lq_core::state::EngineState;
use lq_exchange::spec::InstrumentSpec;
use lq_execution::paper::PaperExecutionVenue;
use lq_execution::positions::PositionManager;
use lq_execution::venue::ExecutionVenue;
use lq_market_data::adapters::{binance, bybit, okx};
use lq_market_data::ws_client::{run_ws, WsConfig};
use lq_orderbook::analytics::{AnalyticsConfig, MarketStateEngine};
use lq_orderbook::engine::BookStore;
use lq_persistence::{PersistenceSink, PostgresStore, RedisHotStateSink};
use lq_risk::{RiskDecision, RiskEngine};
use lq_simulator::{SimulatedFeed, SyntheticDataConfig};
use lq_strategy::{MarketMakingStrategy, StrategyEngine};
use lq_telemetry::{Metrics, MetricsServer};
use lq_types::{Exchange, OrderStatus, OrderType, Symbol};
use rust_decimal_macros::dec;

/// Entry point: build every component and run the event loop.
pub async fn run(cfg: EngineConfig) -> anyhow::Result<()> {
    if cfg.mode == Mode::Live {
        anyhow::bail!(
            "live execution is not implemented in this build; set mode = \"paper\""
        );
    }

    let bus = Arc::new(EventBus::new());
    let state = EngineState::new();
    let metrics = Arc::new(Metrics::new());

    let books = Arc::new(BookStore::new());
    let analytics: DashMap<(Exchange, Symbol), MarketStateEngine> = DashMap::new();
    let venues: DashMap<Exchange, Arc<PaperExecutionVenue>> = DashMap::new();
    let mut strategies = StrategyEngine::new();
    let risk = Arc::new(RiskEngine::new(cfg.risk.clone(), state.clone()));
    let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Wire one instrument pair per configured venue.
    for symbol_str in &cfg.symbols {
        let symbol: Symbol = symbol_str.parse()?;
        for &venue in &cfg.venues {
            let spec = InstrumentSpec::new(dec!(0.1), dec!(0.01));
            books.register(venue, symbol.clone(), spec);
            analytics.insert(
                (venue, symbol.clone()),
                MarketStateEngine::new(venue, symbol.clone(), spec, AnalyticsConfig::default()),
            );
            let paper = Arc::new(PaperExecutionVenue::with_seed(
                venue,
                cfg.paper.clone(),
                Arc::clone(&bus),
                42,
                true,
            ));
            venues.insert(venue, paper);
            strategies.register(Box::new(MarketMakingStrategy::new(
                symbol.clone(),
                venue,
                cfg.strategy.market_making.clone(),
            )));
        }
    }

    // Metrics endpoint.
    {
        let metrics_bind = cfg.telemetry.metrics_bind.clone();
        let server = MetricsServer::new(Arc::clone(&metrics));
        handles.push(tokio::spawn(async move {
            if let Err(e) = server.serve(&metrics_bind).await {
                tracing::error!(err = %e, "metrics server failed");
            }
        }));
    }

    // Control-plane API.
    {
        let api_bind = cfg.api.bind.clone();
        let app = lq_api::build_router(ApiState::new(state.clone(), Arc::clone(&bus)));
        handles.push(tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(&api_bind).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(err = %e, bind = %api_bind, "api server bind failed");
                    return;
                }
            };
            tracing::info!(bind = %api_bind, "api server listening");
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!(err = %e, "api server failed");
            }
        }));
    }

    // Periodic metrics refresh from shared state + bus stats.
    {
        let metrics_state = state.clone();
        let metrics_bus = Arc::clone(&bus);
        let metrics_refresh = Arc::clone(&metrics);
        handles.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                metrics_refresh.observe_state(&metrics_state);
                metrics_refresh.observe_bus(&metrics_bus);
            }
        }));
    }

    // Subscribe before spawning feeds so the boot snapshot is not missed.
    let mut market_sub = bus.market().subscribe();
    let mut exec_sub = bus.execution().subscribe();
    let mut ctrl_sub = bus.control().subscribe();
    let mut running = false;

    // Market data feeds.
    for symbol_str in &cfg.symbols {
        let symbol: Symbol = symbol_str.parse()?;
        for &venue in &cfg.venues {
            let spec = InstrumentSpec::new(dec!(0.1), dec!(0.01));
            match venue {
                Exchange::Paper | Exchange::Simulated => {
                    let feed = SimulatedFeed::new(
                        SyntheticDataConfig {
                            seed: cfg_symbol_seed(&symbol),
                            ..SyntheticDataConfig::default()
                        },
                        venue,
                        symbol.clone(),
                        spec,
                    );
                    handles.push(feed.spawn(Arc::clone(&bus)));
                }
                Exchange::Okx => {
                    let sym = symbol.clone();
                    let feed_bus = Arc::clone(&bus);
                    handles.push(tokio::spawn(async move {
                        if let Err(e) = run_ws(
                            WsConfig {
                                url: okx::OKX_PUBLIC_WS.to_string(),
                                reconnect_base_ms: cfg.market_data.ws_reconnect_base_ms,
                                reconnect_max_ms: cfg.market_data.ws_reconnect_max_ms,
                                stale_after_ms: cfg.market_data.stale_after_ms,
                                ping_interval_ms: cfg.market_data.ping_interval_ms,
                            },
                            Arc::clone(&feed_bus),
                            Box::new(okx::OkxDecoder::new(feed_bus, sym)),
                        )
                        .await
                        {
                            tracing::error!(err = %e, "okx feed failed");
                        }
                    }));
                }
                Exchange::Binance => {
                    let sym = symbol.clone();
                    let feed_bus = Arc::clone(&bus);
                    handles.push(tokio::spawn(async move {
                        if let Err(e) = run_ws(
                            WsConfig {
                                url: binance::stream_url(&sym),
                                reconnect_base_ms: cfg.market_data.ws_reconnect_base_ms,
                                reconnect_max_ms: cfg.market_data.ws_reconnect_max_ms,
                                stale_after_ms: cfg.market_data.stale_after_ms,
                                ping_interval_ms: cfg.market_data.ping_interval_ms,
                            },
                            Arc::clone(&feed_bus),
                            Box::new(binance::BinanceDecoder::new(feed_bus, sym)),
                        )
                        .await
                        {
                            tracing::error!(err = %e, "binance feed failed");
                        }
                    }));
                }
                Exchange::Bybit => {
                    let sym = symbol.clone();
                    let feed_bus = Arc::clone(&bus);
                    handles.push(tokio::spawn(async move {
                        if let Err(e) = run_ws(
                            WsConfig {
                                url: bybit::BYBIT_PUBLIC_WS.to_string(),
                                reconnect_base_ms: cfg.market_data.ws_reconnect_base_ms,
                                reconnect_max_ms: cfg.market_data.ws_reconnect_max_ms,
                                stale_after_ms: cfg.market_data.stale_after_ms,
                                ping_interval_ms: cfg.market_data.ping_interval_ms,
                            },
                            Arc::clone(&feed_bus),
                            Box::new(bybit::BybitDecoder::new(feed_bus, sym)),
                        )
                        .await
                        {
                            tracing::error!(err = %e, "bybit feed failed");
                        }
                    }));
                }
            }
        }
    }

    // Optional durability layer.
    if cfg.persistence.enabled {
        match PostgresStore::connect(&cfg.persistence.postgres_url).await {
            Ok(store) => {
                if let Err(e) = store.migrate().await {
                    tracing::warn!(err = %e, "persistence migration failed");
                }
                let _sink = PersistenceSink::spawn(Arc::clone(&bus), Arc::new(store));
                tracing::info!("persistence enabled");
            }
            Err(e) => tracing::warn!(err = %e, "postgres unavailable; persistence disabled"),
        }

        match RedisHotStateSink::spawn(Arc::clone(&bus), &cfg.persistence.redis_url).await {
            Ok(sink) => {
                let _sink = sink;
                tracing::info!("redis hot state enabled");
            }
            Err(e) => tracing::warn!(err = %e, "redis unavailable; hot state disabled"),
        }
    }

    tracing::info!(symbols = ?cfg.symbols, venues = ?cfg.venues, "trading engine started");

    loop {
        tokio::select! {
            Some(event) = market_sub.recv() => {
                metrics.record_market_event(&event);
                on_market_event(
                    &event,
                    &books,
                    &analytics,
                    &state,
                    &venues,
                    &mut strategies,
                    &risk,
                    &metrics,
                    running,
                ).await;
            }
            Some(event) = exec_sub.recv() => {
                metrics.record_execution(&event);
                state.apply_execution_event(&event);
                PositionManager::on_execution_event(&state, &event);
                strategies.on_execution_event(&event);
            }
            Some(event) = ctrl_sub.recv() => {
                match event {
                    ControlEvent::Start => {
                        strategies.start_all();
                        state.set_strategy_running(true);
                        running = true;
                        tracing::info!("strategies started");
                    }
                    ControlEvent::Stop => {
                        strategies.stop_all();
                        state.set_strategy_running(false);
                        running = false;
                        for ven in venues.iter() {
                            let _ = ven.cancel_all(None).await;
                        }
                        tracing::info!("strategies stopped");
                    }
                    ControlEvent::Reset => {
                        risk.set_kill_switch(false, "reset");
                        sync_risk_state(&state, &risk);
                        tracing::info!("kill switch released");
                    }
                    ControlEvent::KillSwitch { reason } => {
                        risk.set_kill_switch(true, reason);
                        sync_risk_state(&state, &risk);
                        tracing::error!("kill switch engaged");
                    }
                }
            }
        }
    }
}

fn cfg_symbol_seed(symbol: &Symbol) -> u64 {
    symbol.as_str().bytes().fold(0x5EED, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(b as u64)
    })
}

fn sync_risk_state(state: &EngineState, risk: &RiskEngine) {
    let mut status = state.risk_snapshot();
    status.halted = risk.is_halted();
    status.halt_reason = risk.halt_reason();
    status.updated_at = lq_types::TimestampMs::now();
    *state.risk.write() = status;
}

#[allow(clippy::too_many_arguments)]
async fn on_market_event(
    event: &MarketEvent,
    books: &BookStore,
    analytics: &DashMap<(Exchange, Symbol), MarketStateEngine>,
    state: &EngineState,
    venues: &DashMap<Exchange, Arc<PaperExecutionVenue>>,
    strategies: &mut StrategyEngine,
    risk: &RiskEngine,
    metrics: &Metrics,
    running: bool,
) {
    let (venue, symbol) = match event {
        MarketEvent::Snapshot(s) => (s.venue, s.symbol.clone()),
        MarketEvent::Delta(d) => (d.venue, d.symbol.clone()),
        MarketEvent::Trade(t) => (t.venue, t.symbol.clone()),
        MarketEvent::Tick(t) => (t.venue, t.symbol.clone()),
        MarketEvent::Status { venue, symbol, status, .. } => {
            match status {
                FeedStatus::Disconnected => {
                    tracing::warn!(venue = %venue, symbol = %symbol, "feed disconnected; orders suspect");
                    risk.on_venue_reconnecting(*venue);
                    sync_risk_state(state, risk);
                }
                FeedStatus::Healthy | FeedStatus::Resync => {
                    risk.on_venue_connected(*venue);
                    sync_risk_state(state, risk);
                }
                FeedStatus::Stale => {
                    tracing::warn!(venue = %venue, symbol = %symbol, "feed stale");
                }
            }
            return;
        }
    };

    let outcome = books.ingest(event);
    if matches!(outcome, lq_orderbook::engine::IngestOutcome::Gap { .. }) {
        tracing::warn!(venue = %venue, symbol = %symbol, ?outcome, "sequence gap; awaiting resync");
    }

    if let MarketEvent::Trade(trade) = event {
        if let Some(mut engine) = analytics.get_mut(&(venue, symbol.clone())) {
            engine.record_trade(trade.clone());
        }
    }

    let key = (venue, symbol.clone());
    let Some(book) = books.book(venue, &symbol) else {
        return;
    };
    let Some(engine) = analytics.get(&key) else {
        return;
    };
    let ms = engine.compute(&book, event.event_ts());
    state.market_state.insert(key.clone(), ms.clone());

    // Run the strategy on the fresh state and act on its decisions.
    if running && !state.is_halted() {
        let inventory_ref = state.inventory.get(&symbol);
        let inventory = inventory_ref.as_deref();
        let position = state
            .positions
            .get(&(venue, symbol.clone()))
            .map(|p| p.clone());
        let position_ref = position.as_ref();
        let decisions =
            strategies.on_market_state(&ms, inventory, position_ref, state.is_halted(), true);
        for decision in decisions {
            apply_decision(&decision, venues, risk, state).await;
        }
    }

    // Match resting orders against the (possibly moved) book.
    sweep_working_orders(venues, books).await;

    metrics.observe_state(state);
}

async fn apply_decision(
    decision: &StrategyDecision,
    venues: &DashMap<Exchange, Arc<PaperExecutionVenue>>,
    risk: &RiskEngine,
    state: &EngineState,
) {
    match decision {
        StrategyDecision::Quote(intent) => {
            let Some(ven) = venues.get(&intent.venue).map(|v| v.clone()) else {
                return;
            };
            let _ = ven.cancel_all(Some(&intent.symbol)).await;
            for (side, leg) in [
                (lq_types::Side::Bid, intent.bid),
                (lq_types::Side::Ask, intent.ask),
            ] {
                let Some(leg) = leg else {
                    continue;
                };
                place_checked(
                    &ven,
                    side,
                    &intent.symbol,
                    OrderType::Limit,
                    leg.price,
                    leg.qty,
                    risk,
                    state,
                )
                .await;
            }
        }
        StrategyDecision::MarketOrder(signal) => {
            let Some(ven) = venues.get(&signal.venue).map(|v| v.clone()) else {
                return;
            };
            place_checked(
                &ven,
                signal.side,
                &signal.symbol,
                OrderType::Market,
                signal.price,
                signal.qty,
                risk,
                state,
            )
            .await;
        }
        StrategyDecision::StandDown { .. } => {
            for ven in venues.iter() {
                let _ = ven.cancel_all(None).await;
            }
        }
        StrategyDecision::Hold => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn place_checked(
    ven: &PaperExecutionVenue,
    side: lq_types::Side,
    symbol: &Symbol,
    order_type: OrderType,
    price: lq_types::Price,
    qty: lq_types::Qty,
    risk: &RiskEngine,
    state: &EngineState,
) {
    let mark = state
        .market_state
        .get(&(ven.venue(), symbol.clone()))
        .map(|m| m.mid)
        .unwrap_or(price);

    let mut order = Order::new(ven.venue(), symbol.clone(), side, order_type, Some(price), qty);
    match risk.validate_order(&order, mark) {
        RiskDecision::Allow => {
            place(ven, &mut order, state).await;
        }
        RiskDecision::Reduce { qty, .. } => {
            if qty > lq_types::Qty::ZERO {
                order.quantity = qty;
                place(ven, &mut order, state).await;
            }
        }
        RiskDecision::Reject(reason) => {
            tracing::debug!(code = ?reason.code, detail = %reason.detail, "risk reject");
        }
        RiskDecision::Halt(reason) => {
            tracing::error!(detail = %reason.detail, "risk halt");
        }
    }
}

async fn place(ven: &PaperExecutionVenue, order: &mut Order, state: &EngineState) {
    state.orders.insert(order.order_id, order.clone());
    match ven.place_order(order).await {
        Ok(placement) => {
            if placement.status == OrderStatus::Rejected {
                tracing::warn!(order = %order.order_id, "venue rejected order");
            }
        }
        Err(e) => {
            tracing::warn!(order = %order.order_id, err = %e, "order placement failed");
        }
    }
}

/// Fill resting orders whose limit price is now crossed by the book.
async fn sweep_working_orders(
    venues: &DashMap<Exchange, Arc<PaperExecutionVenue>>,
    books: &BookStore,
) {
    for ven in venues.iter() {
        for id in ven.working_order_ids() {
            let Some(order) = ven.order_snapshot(id) else {
                continue;
            };
            if order.status.is_terminal() {
                continue;
            }
            let Some(limit) = order.price else {
                continue;
            };
            let Some(book) = books.book(order.venue, &order.symbol) else {
                continue;
            };
            let Some(best_bid) = book.best_bid() else {
                continue;
            };
            let Some(best_ask) = book.best_ask() else {
                continue;
            };
            let crossed = match order.side {
                lq_types::Side::Bid => best_ask <= limit,
                lq_types::Side::Ask => best_bid >= limit,
            };
            if !crossed {
                continue;
            }
            let remaining = (order.quantity - order.filled_quantity).max(lq_types::Qty::ZERO);
            if remaining <= lq_types::Qty::ZERO {
                continue;
            }
            if let Ok(fill) = ven.report_fill(id, limit, remaining, false).await {
                tracing::info!(
                    venue = %order.venue,
                    symbol = %order.symbol,
                    side = ?order.side,
                    price = %limit,
                    qty = %remaining,
                    fee = %fill.fee,
                    "maker fill"
                );
            }
        }
    }
}