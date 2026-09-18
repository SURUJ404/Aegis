use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::submitter::{SubmitResult, SubmitStatus};

/// Reconciliation state for a submitted transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciledTx {
    pub tx_signature: String,
    pub intent_id: uuid::Uuid,
    pub status: TxStatus,
    pub slot: Option<lq_solana_types::Slot>,
    pub submit_time_ms: u64,
    pub confirm_time_ms: Option<u64>,
    pub compute_units: Option<u64>,
    pub fee_lamports: Option<u64>,
    pub error: Option<String>,
    pub fills: Vec<ReconciledFill>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TxStatus {
    Submitted,
    Confirming,
    Confirmed,
    Failed,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciledFill {
    pub amount_in: u64,
    pub amount_out: u64,
    pub fee: u64,
    pub price: rust_decimal::Decimal,
}

/// Tracks submitted transactions and reconciles them against chain state.
pub struct TxReconciler {
    pending: BTreeMap<String, ReconciledTx>,
    confirmed: BTreeMap<String, ReconciledTx>,
    max_pending: usize,
    pending_ttl_ms: u64,
}

impl TxReconciler {
    pub fn new(max_pending: usize, pending_ttl_ms: u64) -> Self {
        Self {
            pending: BTreeMap::new(),
            confirmed: BTreeMap::new(),
            max_pending,
            pending_ttl_ms,
        }
    }

    pub fn on_submit(
        &mut self,
        intent_id: uuid::Uuid,
        result: &SubmitResult,
        now_ms: u64,
    ) {
        if let Some(sig) = &result.tx_signature {
            let tx = ReconciledTx {
                tx_signature: sig.clone(),
                intent_id,
                status: match result.status {
                    SubmitStatus::Confirmed => TxStatus::Confirmed,
                    SubmitStatus::Failed => TxStatus::Failed,
                    _ => TxStatus::Submitted,
                },
                slot: result.slot,
                submit_time_ms: now_ms,
                confirm_time_ms: if result.status == SubmitStatus::Confirmed {
                    Some(now_ms)
                } else {
                    None
                },
                compute_units: result.compute_units_consumed,
                fee_lamports: result.fee_lamports,
                error: result.error.clone(),
                fills: vec![],
            };

            if tx.status == TxStatus::Confirmed {
                info!(sig, intent_id = %intent_id, "transaction confirmed");
                self.confirmed.insert(sig.clone(), tx);
            } else {
                self.pending.insert(sig.clone(), tx);
            }
        }
    }

    pub fn on_confirm(
        &mut self,
        signature: &str,
        result: &SubmitResult,
        now_ms: u64,
    ) -> Option<ReconciledTx> {
        if let Some(tx) = self.pending.get_mut(signature) {
            match result.status {
                SubmitStatus::Confirmed => {
                    tx.status = TxStatus::Confirmed;
                    tx.confirm_time_ms = Some(now_ms);
                    tx.slot = result.slot;
                    tx.compute_units = result.compute_units_consumed;
                    tx.fee_lamports = result.fee_lamports;
                    let tx = self.pending.remove(signature).unwrap();
                    info!(
                        sig = signature,
                        latency_ms = now_ms - tx.submit_time_ms,
                        "transaction confirmed"
                    );
                    self.confirmed.insert(signature.to_string(), tx.clone());
                    Some(tx)
                }
                SubmitStatus::Failed => {
                    tx.status = TxStatus::Failed;
                    tx.error = result.error.clone();
                    warn!(sig = signature, error = ?result.error, "transaction failed");
                    None
                }
                _ => None,
            }
        } else {
            None
        }
    }

    pub fn expire_stale(&mut self, now_ms: u64) -> Vec<ReconciledTx> {
        let mut expired = vec![];
        let stale: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, tx)| now_ms.saturating_sub(tx.submit_time_ms) > self.pending_ttl_ms)
            .map(|(sig, _)| sig.clone())
            .collect();

        for sig in stale {
            if let Some(mut tx) = self.pending.remove(&sig) {
                tx.status = TxStatus::Expired;
                warn!(sig = sig, "transaction expired");
                expired.push(tx);
            }
        }

        // Enforce max pending limit by dropping oldest
        while self.pending.len() > self.max_pending {
            if let Some((oldest_sig, mut tx)) = self.pending.pop_first() {
                tx.status = TxStatus::Expired;
                warn!(sig = oldest_sig, "transaction evicted (max pending exceeded)");
                expired.push(tx);
            }
        }
        expired
    }

    pub fn get(&self, signature: &str) -> Option<&ReconciledTx> {
        self.pending.get(signature).or_else(|| self.confirmed.get(signature))
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn confirmed_count(&self) -> usize {
        self.confirmed.len()
    }
}

impl Default for TxReconciler {
    fn default() -> Self {
        Self::new(100, 60_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::submitter::SubmitResult;
    use lq_solana_types::Slot;

    #[test]
    fn tracks_submission() {
        let mut reconciler = TxReconciler::new(10, 60_000);
        let intent_id = uuid::Uuid::new_v4();

        let result = SubmitResult {
            tx_signature: Some("sig123".to_string()),
            status: SubmitStatus::Submitted,
            slot: None,
            error: None,
            compute_units_consumed: None,
            fee_lamports: None,
        };

        reconciler.on_submit(intent_id, &result, 1000);
        assert_eq!(reconciler.pending_count(), 1);
        assert!(reconciler.get("sig123").is_some());
    }

    #[test]
    fn confirms_transaction() {
        let mut reconciler = TxReconciler::new(10, 60_000);
        let intent_id = uuid::Uuid::new_v4();

        let submit_result = SubmitResult {
            tx_signature: Some("sig123".to_string()),
            status: SubmitStatus::Submitted,
            slot: None,
            error: None,
            compute_units_consumed: None,
            fee_lamports: None,
        };
        reconciler.on_submit(intent_id, &submit_result, 1000);

        let confirm_result = SubmitResult {
            tx_signature: Some("sig123".to_string()),
            status: SubmitStatus::Confirmed,
            slot: Some(Slot::new(100)),
            error: None,
            compute_units_consumed: Some(50000),
            fee_lamports: Some(5000),
        };
        let confirmed = reconciler.on_confirm("sig123", &confirm_result, 1500);
        assert!(confirmed.is_some());
        assert_eq!(reconciler.pending_count(), 0);
        assert_eq!(reconciler.confirmed_count(), 1);
    }

    #[test]
    fn expires_stale() {
        let mut reconciler = TxReconciler::new(10, 5000);
        let intent_id = uuid::Uuid::new_v4();

        let result = SubmitResult {
            tx_signature: Some("sig123".to_string()),
            status: SubmitStatus::Submitted,
            slot: None,
            error: None,
            compute_units_consumed: None,
            fee_lamports: None,
        };
        reconciler.on_submit(intent_id, &result, 1000);

        let expired = reconciler.expire_stale(8000);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].status, TxStatus::Expired);
    }
}
