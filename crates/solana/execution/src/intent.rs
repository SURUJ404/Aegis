use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use lq_types::{Side, Symbol};
use lq_solana_types::SolanaMarketId;

/// An order intent — the strategy's desired action, transport-agnostic.
///
/// This is the output of the risk engine and the input to the Solana
/// transaction builder. It contains everything needed to construct a
/// transaction without knowing the transport details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIntent {
    pub id: Uuid,
    pub market: SolanaMarketId,
    pub side: Side,
    pub symbol: Symbol,
    /// Token being sold (if selling) or bought (if buying).
    pub token_in_mint: String,
    /// Token being bought (if selling) or sold (if buying).
    pub token_out_mint: String,
    /// Amount in base units (smallest unit, e.g., lamports).
    pub amount_in: u64,
    /// Minimum acceptable output (slippage protection).
    pub min_amount_out: u64,
    /// Maximum price willing to pay (None = market order).
    pub limit_price: Option<Decimal>,
    /// Slippage tolerance in basis points.
    pub slippage_bps: u32,
    /// Time-to-live in seconds.
    pub ttl_secs: u32,
    /// Strategy that generated this intent.
    pub strategy_name: String,
    /// Timestamp when intent was created.
    pub created_at_ms: u64,
}

impl OrderIntent {
    pub fn new(
        market: SolanaMarketId,
        side: Side,
        token_in_mint: impl Into<String>,
        token_out_mint: impl Into<String>,
        amount_in: u64,
        min_amount_out: u64,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            market,
            side,
            symbol: Symbol("SOL-USDC".to_string()),
            token_in_mint: token_in_mint.into(),
            token_out_mint: token_out_mint.into(),
            amount_in,
            min_amount_out,
            limit_price: None,
            slippage_bps: 50, // 0.5% default
            ttl_secs: 30,
            strategy_name: String::new(),
            created_at_ms: 0,
        }
    }

    pub fn with_limit_price(mut self, price: Decimal) -> Self {
        self.limit_price = Some(price);
        self
    }

    pub fn with_slippage_bps(mut self, bps: u32) -> Self {
        self.slippage_bps = bps;
        self
    }

    pub fn with_ttl_secs(mut self, secs: u32) -> Self {
        self.ttl_secs = secs;
        self
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.created_at_ms) > (self.ttl_secs as u64 * 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_solana_types::{SolanaMarketId, SolanaProgram};

    #[test]
    fn intent_creation() {
        let market = SolanaMarketId::new(SolanaProgram::RaydiumAmmV4, "pool123");
        let intent = OrderIntent::new(
            market.clone(),
            Side::Bid,
            "So11111111111111111111111111111111",
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            1_000_000_000,
            150_000_000,
        );
        assert_eq!(intent.side, Side::Bid);
        assert_eq!(intent.amount_in, 1_000_000_000);
        assert!(!intent.is_expired(0));
    }

    #[test]
    fn intent_expiry() {
        let market = SolanaMarketId::new(SolanaProgram::RaydiumAmmV4, "pool123");
        let intent = OrderIntent::new(
            market,
            Side::Bid,
            "SOL",
            "USDC",
            1_000_000_000,
            150_000_000,
        )
        .with_ttl_secs(10);

        assert!(!intent.is_expired(5_000));
        assert!(intent.is_expired(15_001));
    }
}
