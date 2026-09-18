use lq_core::event::market::FeedStatus;
use lq_core::models::book::{OrderBookDelta, OrderBookSnapshot};
use lq_core::models::trade::{MarketTick, Trade};
use lq_core::event::market::MarketEvent;
use lq_types::{Exchange, Symbol, TimestampMs};
use lq_solana_types::SolanaMarketId;

use crate::adapter::MarketDataEvent;

/// Converts normalized Solana events into the Aegis event model.
///
/// The normalizer is stateless — it takes a normalized event and produces
/// Aegis `MarketEvent`s. It does NOT manage sequence numbers or book state.
pub struct SolanaNormalizer {
    /// Solana markets to track (market_id → exchange symbol).
    markets: std::collections::BTreeMap<SolanaMarketId, Symbol>,
}

impl SolanaNormalizer {
    pub fn new() -> Self {
        Self {
            markets: std::collections::BTreeMap::new(),
        }
    }

    pub fn with_markets(mut self, markets: std::collections::BTreeMap<SolanaMarketId, Symbol>) -> Self {
        self.markets = markets;
        self
    }

    pub fn register_market(&mut self, market_id: SolanaMarketId, symbol: Symbol) {
        self.markets.insert(market_id, symbol);
    }

    /// Convert a normalized Solana event into one or more Aegis `MarketEvent`s.
    pub fn normalize(&self, event: &MarketDataEvent) -> Vec<MarketEvent> {
        match event {
            MarketDataEvent::PoolState(pool) => {
                self.normalize_pool_state(pool)
            }
            MarketDataEvent::Swap(trade) => {
                self.normalize_trade(trade)
            }
            MarketDataEvent::LiquidityChange(liq) => {
                self.normalize_liquidity(liq)
            }
            MarketDataEvent::Heartbeat { slot: _, timestamp_ms } => {
                vec![MarketEvent::Status {
                    venue: Exchange::Paper,
                    symbol: Symbol("SOLANA-HEARTBEAT".to_string()),
                    status: FeedStatus::Healthy,
                    ts: TimestampMs(*timestamp_ms),
                }]
            }
        }
    }

    fn symbol_for(&self, market: &SolanaMarketId) -> Symbol {
        self.markets
            .get(market)
            .cloned()
            .unwrap_or_else(|| Symbol(format!("SOL-{}", &market.address[..8.min(market.address.len())])))
    }

    fn normalize_pool_state(
        &self,
        pool: &lq_solana_types::AmmPoolState,
    ) -> Vec<MarketEvent> {
        let symbol = self.symbol_for(&pool.market);
        let mid = pool.mid_price(6, 9).unwrap_or_default();

        let fee_bps = pool.fee_bps().unwrap_or(0.0);
        let half_spread = mid * rust_decimal::Decimal::from((fee_bps / 2.0 * 100.0) as i64)
            / rust_decimal::Decimal::from(10_000);

        let best_bid = mid - half_spread;
        let best_ask = mid + half_spread;

        let mut bids = Vec::new();
        let mut asks = Vec::new();

        bids.push(lq_core::models::book::OrderBookLevel {
            price: best_bid,
            qty: Decimal::from(pool.token_a_reserve) / Decimal::from(1_000_000u64),
        });
        asks.push(lq_core::models::book::OrderBookLevel {
            price: best_ask,
            qty: Decimal::from(pool.token_b_reserve) / Decimal::from(1_000_000_000u64),
        });

        vec![MarketEvent::Snapshot(OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol,
            sequence: pool.slot.as_u64(),
            event_ts: TimestampMs(pool.timestamp_ms),
            exchange_ts: TimestampMs(pool.timestamp_ms),
            bids,
            asks,
        })]
    }

    fn normalize_trade(&self, trade: &lq_solana_types::SolanaTrade) -> Vec<MarketEvent> {
        let symbol = self.symbol_for(&trade.market);

        let aegis_trade = Trade {
            venue: Exchange::Paper,
            symbol: symbol.clone(),
            price: trade.price,
            qty: Decimal::from(trade.amount_out) / Decimal::from(1_000_000_000u64),
            aggressor: lq_types::Side::Ask,
            event_ts: TimestampMs(trade.timestamp_ms),
            exchange_ts: TimestampMs(trade.timestamp_ms),
        };

        let tick = MarketTick {
            venue: Exchange::Paper,
            symbol: symbol.clone(),
            last_price: trade.price,
            last_qty: aegis_trade.qty,
            best_bid: trade.price,
            best_ask: trade.price,
            event_ts: TimestampMs(trade.timestamp_ms),
        };

        vec![
            MarketEvent::Trade(aegis_trade),
            MarketEvent::Tick(tick),
        ]
    }

    fn normalize_liquidity(
        &self,
        liq: &lq_solana_types::SolanaLiquidityChange,
    ) -> Vec<MarketEvent> {
        let symbol = self.symbol_for(&liq.market);

        vec![MarketEvent::Delta(OrderBookDelta {
            venue: Exchange::Paper,
            symbol,
            sequence: liq.slot.as_u64(),
            event_ts: TimestampMs(liq.timestamp_ms),
            exchange_ts: TimestampMs(liq.timestamp_ms),
            changes: vec![],
            clear: false,
        })]
    }
}

use rust_decimal::Decimal;

impl Default for SolanaNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_solana_types::{AmmPoolState, Slot, SolanaMarketId, SolanaProgram, SolanaTrade};

    fn pool(market_addr: &str, slot: u64) -> AmmPoolState {
        AmmPoolState {
            market: SolanaMarketId::new(SolanaProgram::RaydiumAmmV4, market_addr),
            slot: Slot::new(slot),
            token_a_reserve: 1_000_000_000,
            token_b_reserve: 50_000_000_000,
            sqrt_price: None,
            lp_supply: None,
            fee_numerator: 25,
            fee_denominator: 10000,
            timestamp_ms: 1700000000000,
        }
    }

    #[test]
    fn normalize_pool_state_produces_snapshot() {
        let normalizer = SolanaNormalizer::new();
        let event = MarketDataEvent::PoolState(pool("test_pool_addr", 1000));
        let events = normalizer.normalize(&event);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MarketEvent::Snapshot(_)));
    }

    #[test]
    fn normalize_trade_produces_trade_and_tick() {
        let normalizer = SolanaNormalizer::new();
        let trade = SolanaTrade {
            market: SolanaMarketId::new(SolanaProgram::RaydiumAmmV4, "pool"),
            slot: Slot::new(100),
            tx_signature: "sig123".to_string(),
            token_in: "SOL".to_string(),
            token_out: "USDC".to_string(),
            amount_in: 1_000_000_000,
            amount_out: 150_000_000,
            fee_amount: 250_000,
            price: rust_decimal::Decimal::from(150),
            timestamp_ms: 1700000000000,
        };
        let event = MarketDataEvent::Swap(trade);
        let events = normalizer.normalize(&event);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], MarketEvent::Trade(_)));
        assert!(matches!(events[1], MarketEvent::Tick(_)));
    }

    #[test]
    fn normalize_heartbeat_produces_status() {
        let normalizer = SolanaNormalizer::new();
        let event = MarketDataEvent::Heartbeat {
            slot: Slot::new(500),
            timestamp_ms: 1700000000000,
        };
        let events = normalizer.normalize(&event);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MarketEvent::Status { .. }));
    }
}
