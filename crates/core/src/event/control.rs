//! Control events: lifecycle commands.

#[derive(Debug, Clone)]
pub enum ControlEvent {
    /// Start all configured strategies.
    Start,
    /// Stop all strategies and cancel working orders.
    Stop,
    /// Emergency stop; halts all trading. `reason` is always populated.
    KillSwitch { reason: String },
    /// Resume after a halt.
    Reset,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_events_are_cloneable() {
        let e = ControlEvent::KillSwitch {
            reason: "test".into(),
        };
        let _ = e.clone();
    }
}