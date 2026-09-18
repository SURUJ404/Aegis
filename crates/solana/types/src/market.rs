use std::fmt;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::Slot;

/// Solana on-chain market identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SolanaMarketId {
    /// Program that owns this market (e.g., Raydium AMM, OpenBook).
    pub program: crate::SolanaProgram,
    /// Pool/market account address (base58).
    pub address: String,
}

impl SolanaMarketId {
    pub fn new(program: crate::SolanaProgram, address: impl Into<String>) -> Self {
        Self {
            program,
            address: address.into(),
        }
    }
}

impl fmt::Display for SolanaMarketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.program, self.address)
    }
}

/// Source of a market data event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SolanaDataSource {
    /// Yellowstone Geyser gRPC streaming plugin.
    GeyserGrpc,
    /// Solana RPC subscription (polling or websocket).
    RpcSubscribe,
    /// Birdeye or other aggregator API.
    Aggregator,
}

impl fmt::Display for SolanaDataSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GeyserGrpc => write!(f, "geyser_grpc"),
            Self::RpcSubscribe => write!(f, "rpc_subscribe"),
            Self::Aggregator => write!(f, "aggregator"),
        }
    }
}

/// Type of on-chain market event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SolanaMarketEventType {
    /// Full pool state snapshot (reserves, sqrt_price, etc.).
    PoolSnapshot,
    /// Reserve balance change (AMM).
    ReserveChange,
    /// Swap event (trade).
    Swap,
    /// Liquidity add/remove.
    LiquidityChange,
    /// Concentrated liquidity tick update.
    TickUpdate,
    /// Slot heartbeat.
    Heartbeat,
}

impl fmt::Display for SolanaMarketEventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PoolSnapshot => write!(f, "pool_snapshot"),
            Self::ReserveChange => write!(f, "reserve_change"),
            Self::Swap => write!(f, "swap"),
            Self::LiquidityChange => write!(f, "liquidity_change"),
            Self::TickUpdate => write!(f, "tick_update"),
            Self::Heartbeat => write!(f, "heartbeat"),
        }
    }
}

/// A raw Solana market data event before normalization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolanaRawEvent {
    pub slot: Slot,
    pub tx_signature: Option<String>,
    pub program: crate::SolanaProgram,
    pub event_type: SolanaMarketEventType,
    pub source: SolanaDataSource,
    /// Account keys referenced by this event.
    pub accounts: Vec<String>,
    /// Raw event data (protocol-specific bytes, serialized as base64).
    pub data: String,
    /// Wall-clock receive timestamp (ms since epoch).
    pub receive_ts_ms: u64,
    /// On-chain block time if available.
    pub block_time_ms: Option<u64>,
    /// Compute units consumed by the transaction.
    pub compute_units: Option<u64>,
}

/// Normalized AMM pool state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmmPoolState {
    pub market: SolanaMarketId,
    pub slot: Slot,
    pub token_a_reserve: u64,
    pub token_b_reserve: u64,
    pub sqrt_price: Option<u128>,
    pub lp_supply: Option<u64>,
    pub fee_numerator: u64,
    pub fee_denominator: u64,
    pub timestamp_ms: u64,
}

impl AmmPoolState {
    /// Compute mid price from reserves (token_a per token_b, i.e. price of token_a denominated in token_b).
    pub fn mid_price(&self, decimals_a: u8, decimals_b: u8) -> Option<Decimal> {
        if self.token_a_reserve == 0 {
            return None;
        }
        // Price of token_a in token_b = (reserve_b * 10^decimals_a) / (reserve_a * 10^decimals_b)
        let a = Decimal::from(self.token_b_reserve) * Decimal::from(10u64.pow(decimals_a as u32));
        let b = Decimal::from(self.token_a_reserve) * Decimal::from(10u64.pow(decimals_b as u32));
        Some(a / b)
    }

    /// Compute effective fee rate in basis points.
    pub fn fee_bps(&self) -> Option<f64> {
        if self.fee_denominator == 0 {
            return None;
        }
        Some((self.fee_numerator as f64 / self.fee_denominator as f64) * 10_000.0)
    }
}

/// Normalized trade/swap event from a Solana DEX.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolanaTrade {
    pub market: SolanaMarketId,
    pub slot: Slot,
    pub tx_signature: String,
    /// Token being sold.
    pub token_in: String,
    /// Token being bought.
    pub token_out: String,
    pub amount_in: u64,
    pub amount_out: u64,
    pub fee_amount: u64,
    /// Price in terms of token_out per token_in.
    pub price: Decimal,
    pub timestamp_ms: u64,
}

/// Liquidity change event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolanaLiquidityChange {
    pub market: SolanaMarketId,
    pub slot: Slot,
    pub tx_signature: String,
    pub token_a_amount: u64,
    pub token_b_amount: u64,
    pub lp_tokens: Option<u64>,
    pub timestamp_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SolanaProgram;

    #[test]
    fn pool_mid_price() {
        let pool = AmmPoolState {
            market: SolanaMarketId::new(SolanaProgram::RaydiumAmmV4, "test_pool"),
            slot: Slot::new(100),
            token_a_reserve: 1_000_000, // 1 USDC (6 decimals)
            token_b_reserve: 50_000_000, // 0.05 SOL (9 decimals)
            sqrt_price: None,
            lp_supply: None,
            fee_numerator: 25,
            fee_denominator: 10000,
            timestamp_ms: 1700000000000,
        };
        // Price of token_a (USDC) denominated in token_b (SOL):
        // (reserve_b * 10^decimals_a) / (reserve_a * 10^decimals_b) = 0.05
        // Means 1 USDC = 0.05 SOL, i.e. SOL = 50 USDC
        let price = pool.mid_price(6, 9).unwrap();
        assert_eq!(price, Decimal::try_from("0.05").unwrap());
    }

    #[test]
    fn pool_fee_bps() {
        let pool = AmmPoolState {
            market: SolanaMarketId::new(SolanaProgram::RaydiumAmmV4, "test_pool"),
            slot: Slot::new(100),
            token_a_reserve: 1_000_000,
            token_b_reserve: 50_000_000,
            sqrt_price: None,
            lp_supply: None,
            fee_numerator: 25,
            fee_denominator: 10000,
            timestamp_ms: 1700000000000,
        };
        assert!((pool.fee_bps().unwrap() - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn market_id_display() {
        let id = SolanaMarketId::new(SolanaProgram::RaydiumClmm, "pool_addr");
        assert_eq!(id.to_string(), "raydium_clmm:pool_addr");
    }
}
