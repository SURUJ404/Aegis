//! Execution abstraction: venue interface, order state machine, paper
//! execution and position tracking.

pub mod paper;
pub mod positions;
pub mod state_machine;
pub mod venue;

pub use paper::PaperExecutionVenue;
pub use positions::PositionManager;
pub use state_machine::{OrderStateMachine, StateError};
pub use venue::{ExecutionVenue, OrderPlacement, VenueError};