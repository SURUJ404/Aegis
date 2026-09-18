use std::sync::atomic;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use crate::transaction::{BuiltTransaction, TxBuildError};

/// Errors during transaction submission.
#[derive(Debug, Error)]
pub enum SubmitError {
    #[error("transaction build failed: {0}")]
    BuildFailed(#[from] TxBuildError),

    #[error("RPC error: {0}")]
    RpcError(String),

    #[error("transaction rejected: {0}")]
    Rejected(String),

    #[error("blockhash expired")]
    BlockhashExpired,

    #[error("insufficient funds for priority fee")]
    InsufficientFundsForFee,

    #[error("timeout waiting for confirmation")]
    Timeout,

    #[error("network error: {0}")]
    Network(String),
}

/// Result of a transaction submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitResult {
    pub tx_signature: Option<String>,
    pub status: SubmitStatus,
    pub slot: Option<lq_solana_types::Slot>,
    pub error: Option<String>,
    pub compute_units_consumed: Option<u64>,
    pub fee_lamports: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubmitStatus {
    Submitted,
    Confirmed,
    Failed,
    Rejected,
    Timeout,
}

/// Transaction signer abstraction.
#[async_trait::async_trait]
pub trait TransactionSigner: Send + Sync {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SignError>;
    fn pubkey(&self) -> String;
}

#[derive(Debug, Error)]
pub enum SignError {
    #[error("signing failed: {0}")]
    SigningFailed(String),

    #[error("key not available: {0}")]
    KeyUnavailable(String),
}

/// Transaction submission trait.
#[async_trait::async_trait]
pub trait TransactionSubmitter: Send + Sync {
    async fn submit(&self, tx: &BuiltTransaction) -> Result<SubmitResult, SubmitError>;
    async fn confirm(&self, signature: &str, commitment: CommitmentLevel) -> Result<SubmitResult, SubmitError>;
    async fn get_latest_blockhash(&self) -> Result<String, SubmitError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitmentLevel {
    Processed,
    Confirmed,
    Finalized,
}

/// Paper signer — does nothing, used for testing.
pub struct PaperSigner;

impl TransactionSigner for PaperSigner {
    fn sign(&self, _message: &[u8]) -> Result<Vec<u8>, SignError> {
        Ok(vec![0u8; 64])
    }

    fn pubkey(&self) -> String {
        "PaperSigner11111111111111111111111111111111".to_string()
    }
}

/// Paper submitter — records transactions but doesn't submit to any network.
pub struct PaperSubmitter {
    submitted: atomic::AtomicU64,
}

impl PaperSubmitter {
    pub fn new() -> Self {
        Self {
            submitted: atomic::AtomicU64::new(0),
        }
    }

    pub fn submitted_count(&self) -> u64 {
        self.submitted.load(atomic::Ordering::Relaxed)
    }
}

impl Default for PaperSubmitter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl TransactionSubmitter for PaperSubmitter {
    async fn submit(&self, tx: &BuiltTransaction) -> Result<SubmitResult, SubmitError> {
        self.submitted.fetch_add(1, atomic::Ordering::Relaxed);
        debug!(intent_id = %tx.intent_id, "paper: transaction submitted");

        Ok(SubmitResult {
            tx_signature: Some(format!("paper_{}", tx.intent_id)),
            status: SubmitStatus::Confirmed,
            slot: Some(lq_solana_types::Slot::new(1)),
            error: None,
            compute_units_consumed: tx.compute_units,
            fee_lamports: Some(5000),
        })
    }

    async fn confirm(&self, signature: &str, _commitment: CommitmentLevel) -> Result<SubmitResult, SubmitError> {
        Ok(SubmitResult {
            tx_signature: Some(signature.to_string()),
            status: SubmitStatus::Confirmed,
            slot: Some(lq_solana_types::Slot::new(1)),
            error: None,
            compute_units_consumed: None,
            fee_lamports: Some(5000),
        })
    }

    async fn get_latest_blockhash(&self) -> Result<String, SubmitError> {
        Ok("paper_blockhash_11111111111111111111111111111111".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::TransactionBuilder;
    use lq_solana_types::{SolanaMarketId, SolanaProgram};
    use lq_types::Side;

    #[tokio::test]
    async fn paper_submitter_records() {
        let submitter = PaperSubmitter::new();
        let builder = crate::transaction::PassthroughBuilder;
        let intent = crate::intent::OrderIntent::new(
            SolanaMarketId::new(SolanaProgram::RaydiumAmmV4, "pool"),
            Side::Bid,
            "SOL",
            "USDC",
            1_000_000_000,
            150_000_000,
        );
        let tx = builder.build(&intent, "blockhash").unwrap();
        let result = submitter.submit(&tx).await.unwrap();
        assert_eq!(result.status, SubmitStatus::Confirmed);
        assert_eq!(submitter.submitted_count(), 1);
    }

    #[test]
    fn paper_signer_produces_signature() {
        let signer = PaperSigner;
        let sig = signer.sign(b"test message").unwrap();
        assert_eq!(sig.len(), 64);
    }
}
