//! Strategy trait and engine.
//!
//! Strategies are pure domain logic: they consume a [`MarketState`] and their
//! own configuration, and produce [`StrategyDecision`]s. They know nothing
//! about networking, databases or exchange implementations. Everything a
//! strategy is allowed to see is in [`StrategyContext`].

use lq_core::event::ExecutionEvent;
use lq_core::models::{Inventory, MarketState, Position, StrategyDecision};
use lq_types::TimestampMs;

pub mod cross_venue;
pub mod market_making;

pub use cross_venue::{CrossVenueAnalyzer, CrossVenueConfig, CrossVenueOpportunity};
pub use market_making::MarketMakingStrategy;

/// Everything a strategy may observe about the world when making a decision.
#[derive(Debug, Clone)]
pub struct StrategyContext<'a> {
    /// Per-symbol aggregate inventory (across venues).
    pub inventory: Option<Inventory>,
    /// Per-venue position for this symbol, if one exists.
    pub position: Option<Position>,
    /// Whether the risk engine has halted trading.
    pub halted: bool,
    /// Whether strategies are active (started).
    pub running: bool,
    pub now: TimestampMs,
    pub market: &'a MarketState,
}

/// A trading strategy. Implementations must be deterministic given the same
/// input sequence (this is what makes backtesting meaningful).
pub trait Strategy: Send + Sync {
    fn name(&self) -> &'static str;

    /// Produce a decision for the current market state.
    fn on_market_state(&mut self, ctx: &StrategyContext) -> StrategyDecision;

    /// Receive execution events (fills, rejects) for internal bookkeeping.
    fn on_execution_event(&mut self, _event: &ExecutionEvent) {}

    fn on_start(&mut self) {}
    fn on_stop(&mut self) {}
}

/// Aggregates multiple strategies into one decision-producing unit. Also runs
/// cross-venue analysis when more than one venue publishes a state for the
/// same symbol.
#[derive(Default)]
pub struct StrategyEngine {
    strategies: Vec<Box<dyn Strategy>>,
    cross_venue: Option<CrossVenueAnalyzer>,
    venues_by_symbol: std::collections::HashMap<lq_types::Symbol, Vec<MarketState>>,
}

impl StrategyEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, strategy: Box<dyn Strategy>) {
        self.strategies.push(strategy);
    }

    pub fn set_cross_venue_analyzer(&mut self, analyzer: CrossVenueAnalyzer) {
        self.cross_venue = Some(analyzer);
    }

    pub fn is_empty(&self) -> bool {
        self.strategies.is_empty()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.strategies.iter().map(|s| s.name()).collect()
    }

    pub fn start_all(&mut self) {
        for s in &mut self.strategies {
            s.on_start();
        }
    }

    pub fn stop_all(&mut self) {
        for s in &mut self.strategies {
            s.on_stop();
        }
    }

    /// Feed a market state update and collect all decisions.
    pub fn on_market_state(
        &mut self,
        market: &MarketState,
        inventory: Option<&Inventory>,
        position: Option<&Position>,
        halted: bool,
        running: bool,
    ) -> Vec<StrategyDecision> {
        let mut decisions = Vec::new();

        let ctx = StrategyContext {
            inventory: inventory.cloned(),
            position: position.cloned(),
            halted,
            running,
            now: market.event_ts,
            market,
        };

        for s in &mut self.strategies {
            decisions.push(s.on_market_state(&ctx));
        }

        if let Some(analyzer) = &self.cross_venue {
            let states = self
                .venues_by_symbol
                .entry(market.symbol.clone())
                .or_default();
            // Replace this venue's cached state.
            states.retain(|s| s.venue != market.venue);
            states.push(market.clone());
            if states.len() >= 2 {
                if let Some(opportunity) = analyzer.analyze(states) {
                    if let Some(decision) = opportunity.to_decision() {
                        decisions.push(decision);
                    }
                }
            }
        }

        decisions
    }

    pub fn on_execution_event(&mut self, event: &ExecutionEvent) {
        for s in &mut self.strategies {
            s.on_execution_event(event);
        }
    }
}