//! PostgreSQL durable store.

use lq_core::event::{ExecutionEvent, MarketEvent};
use sqlx::postgres::PgPoolOptions;

use crate::store::{ExecutionStore, MarketDataStore, StoreError};

/// A PostgreSQL-backed store for market and execution data.
#[derive(Clone)]
pub struct PostgresStore {
    pool: sqlx::PgPool,
}

impl PostgresStore {
    /// Connect to the database.
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    /// Create the schema if it does not exist.
    pub async fn migrate(&self) -> Result<(), StoreError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS market_data (
                id       BIGSERIAL PRIMARY KEY,
                venue    TEXT NOT NULL,
                symbol   TEXT NOT NULL,
                kind     TEXT NOT NULL,
                seq      BIGINT NOT NULL DEFAULT 0,
                ts       BIGINT NOT NULL,
                payload  TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS executions (
                execution_id UUID PRIMARY KEY,
                order_id     UUID NOT NULL,
                venue        TEXT NOT NULL,
                symbol       TEXT NOT NULL,
                exec_type    TEXT NOT NULL,
                price        NUMERIC NOT NULL,
                qty          NUMERIC NOT NULL,
                fee          NUMERIC NOT NULL,
                fee_currency TEXT NOT NULL,
                exchange_ts  BIGINT NOT NULL,
                event_ts     BIGINT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS order_events (
                id       BIGSERIAL PRIMARY KEY,
                order_id UUID NOT NULL,
                venue    TEXT NOT NULL,
                kind     TEXT NOT NULL,
                ts       BIGINT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS market_data_venue_symbol_ts
                ON market_data (venue, symbol, ts);
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl MarketDataStore for PostgresStore {
    async fn save_market_event(&self, event: &MarketEvent) -> Result<(), StoreError> {
        let (venue, symbol, seq, ts, payload) = match event {
            MarketEvent::Snapshot(s) => (
                s.venue.to_string(),
                s.symbol.as_str().to_string(),
                s.sequence as i64,
                s.event_ts.as_u64() as i64,
                serde_json::to_string(s)?,
            ),
            MarketEvent::Delta(d) => (
                d.venue.to_string(),
                d.symbol.as_str().to_string(),
                d.sequence as i64,
                d.event_ts.as_u64() as i64,
                serde_json::to_string(d)?,
            ),
            MarketEvent::Trade(t) => (
                t.venue.to_string(),
                t.symbol.as_str().to_string(),
                0,
                t.event_ts.as_u64() as i64,
                serde_json::to_string(t)?,
            ),
            MarketEvent::Tick(t) => (
                t.venue.to_string(),
                t.symbol.as_str().to_string(),
                0,
                t.event_ts.as_u64() as i64,
                serde_json::to_string(t)?,
            ),
            MarketEvent::Status { venue, symbol, status, ts } => {
                let payload = serde_json::json!({
                    "venue": venue,
                    "symbol": symbol.as_str(),
                    "status": format!("{status:?}"),
                    "ts": ts.as_u64(),
                });
                (
                    venue.to_string(),
                    symbol.as_str().to_string(),
                    0,
                    ts.as_u64() as i64,
                    payload.to_string(),
                )
            }
        };
        let _ = seq;

        sqlx::query(
            "INSERT INTO market_data (venue, symbol, kind, seq, ts, payload) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(venue)
        .bind(symbol)
        .bind(kind_str(event))
        .bind(seq)
        .bind(ts)
        .bind(payload)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn kind_str(event: &MarketEvent) -> &'static str {
    match event {
        MarketEvent::Snapshot(_) => "snapshot",
        MarketEvent::Delta(_) => "delta",
        MarketEvent::Trade(_) => "trade",
        MarketEvent::Tick(_) => "tick",
        MarketEvent::Status { .. } => "status",
    }
}

#[async_trait::async_trait]
impl ExecutionStore for PostgresStore {
    async fn save_execution_event(&self, event: &ExecutionEvent) -> Result<(), StoreError> {
        match event {
            ExecutionEvent::Fill(fill) => {
                sqlx::query(
                    r#"
                    INSERT INTO executions
                        (execution_id, order_id, venue, symbol, exec_type, price, qty, fee,
                         fee_currency, exchange_ts, event_ts)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                    "#,
                )
                .bind(fill.execution_id)
                .bind(fill.order_id)
                .bind(fill.venue.to_string())
                .bind(fill.symbol.as_str())
                .bind("fill")
                .bind(fill.price)
                .bind(fill.qty)
                .bind(fill.fee)
                .bind(&fill.fee_currency)
                .bind(fill.exchange_ts.as_u64() as i64)
                .bind(fill.event_ts.as_u64() as i64)
                .execute(&self.pool)
                .await?;
            }
            other => {
                let ts = match other {
                    ExecutionEvent::New { ts, .. }
                    | ExecutionEvent::Acknowledged { ts, .. }
                    | ExecutionEvent::CancelRequested { ts, .. }
                    | ExecutionEvent::Cancelled { ts, .. }
                    | ExecutionEvent::Rejected { ts, .. }
                    | ExecutionEvent::Expired { ts, .. } => *ts,
                    ExecutionEvent::Fill(f) => f.event_ts,
                    ExecutionEvent::Trade { ts, .. } => *ts,
                };
                sqlx::query(
                    "INSERT INTO order_events (order_id, venue, kind, ts) VALUES ($1, $2, $3, $4)",
                )
                .bind(other.order_id().unwrap_or_default())
                .bind(other.venue().to_string())
                .bind(execution_kind(other))
                .bind(ts.as_u64() as i64)
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(())
    }
}

fn execution_kind(event: &ExecutionEvent) -> &'static str {
    match event {
        ExecutionEvent::New { .. } => "new",
        ExecutionEvent::Acknowledged { .. } => "acknowledged",
        ExecutionEvent::Fill(_) => "fill",
        ExecutionEvent::CancelRequested { .. } => "cancel_requested",
        ExecutionEvent::Cancelled { .. } => "cancelled",
        ExecutionEvent::Rejected { .. } => "rejected",
        ExecutionEvent::Expired { .. } => "expired",
        ExecutionEvent::Trade { .. } => "trade",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::{OrderBookDelta, OrderBookSnapshot, Trade};
    use lq_types::{Exchange, Side, Symbol, TimestampMs};
    use rust_decimal_macros::dec;

    #[test]
    fn sql_binds_serialize_events() {
        let d = OrderBookDelta {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: 1,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
            changes: vec![lq_core::models::LevelChange {
                side: Side::Bid,
                price: dec!(100),
                qty: dec!(1),
            }],
            clear: false,
        };
        let s = OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: 1,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
            bids: vec![],
            asks: vec![],
        };
        let t = Trade {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            price: dec!(100),
            qty: dec!(0.1),
            aggressor: Side::Bid,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
        };
        assert_eq!(kind_str(&MarketEvent::Delta(d.clone())), "delta");
        assert_eq!(kind_str(&MarketEvent::Snapshot(s.clone())), "snapshot");
        assert_eq!(kind_str(&MarketEvent::Trade(t.clone())), "trade");
        assert!(serde_json::to_string(&d).is_ok());
        assert!(serde_json::to_string(&s).is_ok());
        assert!(serde_json::to_string(&t).is_ok());
    }

    #[test]
    fn execution_kind_names() {
        let e = ExecutionEvent::Cancelled {
            order_id: uuid::Uuid::new_v4(),
            venue: Exchange::Paper,
            ts: TimestampMs(1),
        };
        assert_eq!(execution_kind(&e), "cancelled");
    }
}