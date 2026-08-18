//! Paper exchange and synthetic market data.
//!
//! Two roles:
//! - [`SyntheticMarketData`] produces a deterministic random-walk order book
//!   (snapshots, deltas, trades) for simulation and demos.
//! - [`PaperExchange`] connects a [`PaperExecutionVenue`] to a local book and
//!   matches resting orders whenever the simulated book crosses them.

pub mod exchange;
pub mod market_gen;

pub use exchange::PaperExchange;
pub use market_gen::{SimulatedFeed, SyntheticDataConfig, SyntheticMarketData};