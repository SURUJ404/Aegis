use anyhow::Result;
use lq_types::Side;
use rust_decimal::Decimal;
use solana_sdk::pubkey::Pubkey;
use std::sync::Mutex;

use crate::models::{
    NormalizedSolanaEvent, SolanaEventMetadata, SolanaEventPayload,
    SolanaEventType, SolanaMarket, SolanaProtocol, OrderBookLevelData,
};

/// Decodes Solana account/data changes into normalized events
pub struct SolanaEventDecoder {
    market: SolanaMarket,
    sequence: Mutex<u64>,
    last_slot: Mutex<u64>,
}

impl SolanaEventDecoder {
    pub fn new(market: SolanaMarket) -> Self {
        Self {
            market,
            sequence: Mutex::new(0),
            last_slot: Mutex::new(0),
        }
    }

    /// Decode a Geyser account update
    pub fn decode_account_update(
        &self,
        slot: u64,
        signature: &str,
        account_data: &[u8],
        program_id: &Pubkey,
    ) -> Result<Option<NormalizedSolanaEvent>> {
        *self.last_slot.lock().unwrap() = slot;
        *self.sequence.lock().unwrap() += 1;

        match self.market.protocol {
            SolanaProtocol::RaydiumClmm => self.decode_raydium_clmm(slot, signature, account_data, program_id),
            SolanaProtocol::OrcaWhirlpools => self.decode_orca_whirlpool(slot, signature, account_data, program_id),
            SolanaProtocol::Phoenix => self.decode_phoenix(slot, signature, account_data, program_id),
            SolanaProtocol::OpenBook => self.decode_openbook(slot, signature, account_data, program_id),
            _ => Ok(None),
        }
    }

    /// Decode a transaction log (from logsSubscribe)
    pub fn decode_transaction_log(
        &self,
        slot: u64,
        signature: &str,
        logs: &[String],
    ) -> Result<Option<NormalizedSolanaEvent>> {
        *self.last_slot.lock().unwrap() = slot;
        *self.sequence.lock().unwrap() += 1;

        // Parse logs for swap events
        for log in logs {
            if log.contains("Swap") || log.contains("swap") {
                return self.parse_swap_log(slot, signature, log);
            }
        }
        Ok(None)
    }

    fn metadata(&self, slot: u64, signature: &str, program_id: &Pubkey) -> SolanaEventMetadata {
        SolanaEventMetadata {
            slot,
            signature: signature.to_string(),
            program_id: program_id.to_string(),
            receive_ts: lq_types::TimestampMs::now(),
            source_ts: None,
            sequence: *self.sequence.lock().unwrap(),
        }
    }

    fn decode_raydium_clmm(
        &self,
        slot: u64,
        signature: &str,
        data: &[u8],
        program_id: &Pubkey,
    ) -> Result<Option<NormalizedSolanaEvent>> {
        // Raydium CLMM pool state layout parsing
        // This is a simplified version - real implementation would use borsh
        if data.len() < 100 {
            return Ok(None);
        }

        // Pool state has sqrt_price_x64 at offset 73, liquidity at offset 81, etc.
        // For now, return a placeholder tick event
        Ok(Some(NormalizedSolanaEvent {
            metadata: self.metadata(slot, signature, program_id),
            market: self.market.clone(),
            event_type: SolanaEventType::Tick,
            payload: SolanaEventPayload::Tick {
                price: Decimal::ZERO,
                qty: Decimal::ZERO,
                best_bid: Decimal::ZERO,
                best_ask: Decimal::ZERO,
            },
        }))
    }

    fn decode_orca_whirlpool(
        &self,
        slot: u64,
        signature: &str,
        data: &[u8],
        program_id: &Pubkey,
    ) -> Result<Option<NormalizedSolanaEvent>> {
        // Orca Whirlpool state parsing
        if data.len() < 100 {
            return Ok(None);
        }

        Ok(Some(NormalizedSolanaEvent {
            metadata: self.metadata(slot, signature, program_id),
            market: self.market.clone(),
            event_type: SolanaEventType::Tick,
            payload: SolanaEventPayload::Tick {
                price: Decimal::ZERO,
                qty: Decimal::ZERO,
                best_bid: Decimal::ZERO,
                best_ask: Decimal::ZERO,
            },
        }))
    }

    fn decode_phoenix(
        &self,
        slot: u64,
        signature: &str,
        data: &[u8],
        program_id: &Pubkey,
    ) -> Result<Option<NormalizedSolanaEvent>> {
        // Phoenix orderbook parsing - has actual order book structure
        if data.len() < 200 {
            return Ok(None);
        }

        Ok(Some(NormalizedSolanaEvent {
            metadata: self.metadata(slot, signature, program_id),
            market: self.market.clone(),
            event_type: SolanaEventType::OrderBookDelta,
            payload: SolanaEventPayload::OrderBookDelta {
                changes: vec![],
            },
        }))
    }

    fn decode_openbook(
        &self,
        slot: u64,
        signature: &str,
        _data: &[u8],
        program_id: &Pubkey,
    ) -> Result<Option<NormalizedSolanaEvent>> {
        // OpenBook (Serum v3) parsing
        Ok(Some(NormalizedSolanaEvent {
            metadata: self.metadata(slot, signature, program_id),
            market: self.market.clone(),
            event_type: SolanaEventType::OrderBookDelta,
            payload: SolanaEventPayload::OrderBookDelta {
                changes: vec![],
            },
        }))
    }

    fn parse_swap_log(&self, slot: u64, signature: &str, log: &str) -> Result<Option<NormalizedSolanaEvent>> {
        // Parse Raydium/Orca swap logs
        // Example: "Program log: Swap: amount_in=1000000 amount_out=950000"
        let meta = self.metadata(slot, signature, &self.market.pool_address.parse().unwrap());

        // Try to extract amounts
        let (amount_in, amount_out) = Self::extract_amounts(log).unwrap_or((0, 0));
        if amount_in == 0 && amount_out == 0 {
            return Ok(None);
        }

        Ok(Some(NormalizedSolanaEvent {
            metadata: meta,
            market: self.market.clone(),
            event_type: SolanaEventType::Swap,
            payload: SolanaEventPayload::Swap {
                price: Decimal::from(amount_out) / Decimal::from(amount_in.max(1)),
                qty: Decimal::from(amount_in),
                side: Side::Bid, // Would need to determine from log
                input_mint: String::new(),
                output_mint: String::new(),
            },
        }))
    }

    fn extract_amounts(log: &str) -> Option<(u64, u64)> {
        // Simple regex-like extraction for "amount_in=X amount_out=Y"
        let parts: Vec<&str> = log.split_whitespace().collect();
        let mut amount_in = 0u64;
        let mut amount_out = 0u64;

        for part in parts {
            if let Some(stripped) = part.strip_prefix("amount_in=") {
                amount_in = stripped.parse().ok()?;
            } else if let Some(stripped) = part.strip_prefix("amount_out=") {
                amount_out = stripped.parse().ok()?;
            }
        }

        Some((amount_in, amount_out))
    }
}

/// Helper to create level data from ticks
pub fn ticks_to_levels(
    sqrt_price_x64: u128,
    _tick_spacing: u16,
    _tick_array_bitmap: &[u64],
    _tick_arrays: &[Vec<u8>],
) -> (Vec<OrderBookLevelData>, Vec<OrderBookLevelData>) {
    // Convert sqrt_price_x64 to price
    // This is a placeholder - real implementation would parse tick arrays
    let price = Decimal::from_str_exact(&format!("{}", sqrt_price_x64 as f64 / 2_f64.powi(64))).unwrap_or(Decimal::ZERO);
    let bids = vec![OrderBookLevelData { price, qty: Decimal::from(1) }];
    let asks = vec![OrderBookLevelData { price: price * Decimal::from(10001) / Decimal::from(10000), qty: Decimal::from(1) }];
    (bids, asks)
}