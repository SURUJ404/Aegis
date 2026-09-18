use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::intent::OrderIntent;

/// Errors during transaction construction.
#[derive(Debug, Error)]
pub enum TxBuildError {
    #[error("missing account: {0}")]
    MissingAccount(String),

    #[error("invalid instruction data: {0}")]
    InvalidInstruction(String),

    #[error("insufficient funds: need {need}, have {have}")]
    InsufficientFunds { need: u64, have: u64 },

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("unsupported program: {0}")]
    UnsupportedProgram(String),
}

/// A built transaction ready for signing and submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltTransaction {
    /// The serialized transaction message (unsigned).
    pub message_bytes: Vec<u8>,
    /// Account keys in the order they appear in the transaction.
    pub account_keys: Vec<String>,
    /// Instructions to execute.
    pub instructions: Vec<BuiltInstruction>,
    /// Recent blockhash for validity.
    pub recent_blockhash: String,
    /// The original order intent this transaction fulfills.
    pub intent_id: uuid::Uuid,
    /// Estimated compute units.
    pub compute_units: Option<u64>,
    /// Priority fee in microlamports per compute unit.
    pub priority_fee: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltInstruction {
    pub program_id: String,
    pub accounts: Vec<AccountMeta>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountMeta {
    pub pubkey: String,
    pub is_signer: bool,
    pub is_writable: bool,
}

/// Builds a Solana transaction from an order intent.
///
/// The transaction builder is protocol-specific — different DEX programs
/// require different instruction formats. This trait abstracts that.
pub trait TransactionBuilder: Send + Sync {
    /// Build a transaction from an order intent.
    fn build(&self, intent: &OrderIntent, recent_blockhash: &str) -> Result<BuiltTransaction, TxBuildError>;

    /// Compute the estimated compute units for this transaction.
    fn estimate_compute_units(&self, intent: &OrderIntent) -> u64;
}

/// A pass-through builder that produces a minimal transaction.
/// Used for testing and as a fallback for unsupported protocols.
pub struct PassthroughBuilder;

impl TransactionBuilder for PassthroughBuilder {
    fn build(&self, intent: &OrderIntent, recent_blockhash: &str) -> Result<BuiltTransaction, TxBuildError> {
        // For unsupported protocols, build a placeholder transaction
        // that logs the intent but doesn't actually execute.
        Ok(BuiltTransaction {
            message_bytes: vec![],
            account_keys: vec![intent.token_in_mint.clone(), intent.token_out_mint.clone()],
            instructions: vec![],
            recent_blockhash: recent_blockhash.to_string(),
            intent_id: intent.id,
            compute_units: Some(200_000),
            priority_fee: Some(10_000),
        })
    }

    fn estimate_compute_units(&self, _intent: &OrderIntent) -> u64 {
        200_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_solana_types::{SolanaMarketId, SolanaProgram};
    use lq_types::Side;

    #[test]
    fn passthrough_builds_placeholder() {
        let builder = PassthroughBuilder;
        let intent = crate::intent::OrderIntent::new(
            SolanaMarketId::new(SolanaProgram::RaydiumAmmV4, "pool"),
            Side::Bid,
            "SOL",
            "USDC",
            1_000_000_000,
            150_000_000,
        );
        let tx = builder.build(&intent, "blockhash123").unwrap();
        assert_eq!(tx.recent_blockhash, "blockhash123");
        assert_eq!(tx.intent_id, intent.id);
    }
}
