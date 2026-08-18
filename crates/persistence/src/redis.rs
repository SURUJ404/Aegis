//! Redis hot-state layer.
//!
//! Lives outside the hot path: the engine writes cheap scalars (last price,
//! halt flag, working-order counts) so the control API and dashboards can read
//! them without touching shared engine state.

use lq_types::Symbol;
use redis::aio::MultiplexedConnection;

use crate::store::StoreError;

/// Redis-backed hot state. Cheap to clone (the underlying connection is
/// multiplexed); methods require `&mut self` for the underlying connection
/// protocol.
#[derive(Clone)]
pub struct RedisHotState {
    con: MultiplexedConnection,
}

impl RedisHotState {
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let client = redis::Client::open(url)?;
        let con = client.get_multiplexed_tokio_connection().await?;
        Ok(Self { con })
    }

    fn price_key(symbol: &Symbol) -> String {
        format!("lq:last_price:{}", symbol.as_str())
    }

    pub async fn set_last_price(&mut self, symbol: &Symbol, price: f64) -> Result<(), StoreError> {
        redis::cmd("SET")
            .arg(Self::price_key(symbol))
            .arg(price.to_string())
            .query_async::<()>(&mut self.con)
            .await?;
        Ok(())
    }

    pub async fn get_last_price(&mut self, symbol: &Symbol) -> Result<Option<f64>, StoreError> {
        let raw: Option<String> = redis::cmd("GET")
            .arg(Self::price_key(symbol))
            .query_async(&mut self.con)
            .await?;
        Ok(raw.and_then(|s| s.parse().ok()))
    }

    pub async fn set_halted(&mut self, halted: bool) -> Result<(), StoreError> {
        redis::cmd("SET")
            .arg("lq:halted")
            .arg(if halted { "1" } else { "0" })
            .query_async::<()>(&mut self.con)
            .await?;
        Ok(())
    }

    pub async fn get_halted(&mut self) -> Result<bool, StoreError> {
        let raw: Option<String> = redis::cmd("GET")
            .arg("lq:halted")
            .query_async(&mut self.con)
            .await?;
        Ok(raw.as_deref() == Some("1"))
    }

    /// Increment the working-order count for a venue by `delta`.
    pub async fn adjust_open_orders(
        &mut self,
        venue: &str,
        delta: i64,
    ) -> Result<i64, StoreError> {
        let n: i64 = redis::cmd("INCRBY")
            .arg(format!("lq:open_orders:{venue}"))
            .arg(delta)
            .query_async(&mut self.con)
            .await?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_namespaced() {
        assert_eq!(
            RedisHotState::price_key(&Symbol("BTC-USDT".into())),
            "lq:last_price:BTC-USDT"
        );
    }
}