//! Performance metrics computed from a backtest equity curve.
//!
//! Everything here is a pure function of the input samples; the definitions
//! are documented inline so the numbers are auditable.

use lq_types::Amount;
use serde::Serialize;

/// One mark-to-market snapshot of the account.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct EquitySample {
    /// Cumulative market events processed so far.
    pub events: u64,
    /// Account equity at this sample (realized + mark-to-market).
    pub equity: Amount,
}

/// Summary metrics for a run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PerfMetrics {
    /// Total net PnL (realized + mark-to-market at the last mark).
    pub net_pnl: Amount,
    /// Total fees paid across all fills.
    pub fees_total: Amount,
    /// Number of fills.
    pub fills: u64,
    /// Number of round-trip trades detected (closing fills).
    pub trades: u64,
    /// Fraction of closing fills that added to realized PnL.
    pub win_rate: f64,
    /// Largest peak-to-trough equity decline, in quote currency.
    pub max_drawdown: Amount,
    /// Largest peak-to-trough equity decline as a fraction of the peak.
    pub max_drawdown_pct: f64,
    /// Annualized Sharpe ratio of interval returns.
    ///
    /// `sharpe = mean(r) / std(r) * sqrt(periods_per_year)`, where `r` are
    /// the per-sample returns of the equity curve.
    pub sharpe: f64,
    /// Final equity (last sample).
    pub final_equity: Amount,
}

/// Compute metrics from a sampled equity curve.
pub fn compute(samples: &[EquitySample], periods_per_year: f64) -> PerfMetrics {
    let mut m = PerfMetrics::default();
    if samples.is_empty() {
        return m;
    }

    let mut peak = samples[0].equity;
    let mut max_dd = Amount::ZERO;
    for s in samples {
        if s.equity > peak {
            peak = s.equity;
        }
        let dd = peak - s.equity;
        if dd > max_dd {
            max_dd = dd;
        }
    }
    m.max_drawdown = max_dd;
    m.max_drawdown_pct = if peak.is_zero() {
        0.0
    } else {
        (max_dd / peak).as_f64()
    };
    m.final_equity = samples.last().map(|s| s.equity).unwrap_or_default();
    m.net_pnl = m.final_equity - samples[0].equity;
    m.sharpe = sharpe(samples, periods_per_year);
    m
}

fn to_f64(a: Amount) -> f64 {
    a.as_f64()
}

fn sharpe(samples: &[EquitySample], periods_per_year: f64) -> f64 {
    if samples.len() < 3 {
        return 0.0;
    }
    let mut returns = Vec::with_capacity(samples.len() - 1);
    for w in samples.windows(2) {
        let prev = to_f64(w[0].equity);
        let cur = to_f64(w[1].equity);
        if prev > 0.0 {
            returns.push((cur - prev) / prev);
        }
    }
    if returns.len() < 2 {
        return 0.0;
    }
    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let var = returns.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / (n - 1.0);
    if var <= 0.0 {
        return 0.0;
    }
    let std = var.sqrt();
    mean / std * periods_per_year.max(1.0).sqrt()
}

/// Aggregate a full backtest result (metrics + execution counts).
#[derive(Debug, Clone, Default, Serialize)]
pub struct BacktestResult {
    pub events_seen: u64,
    pub orders_placed: u64,
    pub rejected_orders: u64,
    pub open_orders_at_end: usize,
    pub metrics: PerfMetrics,
    /// Reject counts by risk-code name, e.g. `max_order_rate -> 13314`.
    /// Empty when nothing was rejected.
    pub rejects_by_code: std::collections::BTreeMap<String, u64>,
    /// Mark-to-market equity curve (sampled per `equity_sample_every` events).
    pub equity_curve: Vec<EquitySample>,
}

impl BacktestResult {
    /// Update the result with an equity sample; called by the runner.
    pub fn with_metrics(mut self, metrics: PerfMetrics) -> Self {
        self.metrics = metrics;
        self
    }
}

