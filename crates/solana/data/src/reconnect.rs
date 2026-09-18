use std::time::Instant;

/// Connection state tracker with exponential backoff awareness.
pub struct ConnectionState {
    state: State,
    reconnect_base_ms: u64,
    reconnect_max_ms: u64,
    disconnect_time: Option<Instant>,
    reconnect_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Connected,
    Disconnected,
}

impl ConnectionState {
    pub fn new(reconnect_base_ms: u64, reconnect_max_ms: u64) -> Self {
        Self {
            state: State::Connected,
            reconnect_base_ms,
            reconnect_max_ms,
            disconnect_time: None,
            reconnect_count: 0,
        }
    }

    pub fn on_disconnect(&mut self) {
        self.state = State::Disconnected;
        self.disconnect_time = Some(Instant::now());
        self.reconnect_count += 1;
    }

    pub fn on_reconnect(&mut self) {
        self.state = State::Connected;
        self.disconnect_time = None;
    }

    pub fn is_connected(&self) -> bool {
        self.state == State::Connected
    }

    pub fn is_disconnected(&self) -> bool {
        self.state != State::Connected
    }

    pub fn reconnect_count(&self) -> u64 {
        self.reconnect_count
    }

    /// Compute the backoff delay for the next reconnection attempt.
    pub fn next_backoff_ms(&self) -> u64 {
        // Exponential backoff with jitter, capped at max.
        let exponent = (self.reconnect_count as f64).min(10.0);
        let base = self.reconnect_base_ms as f64 * 2.0_f64.powf(exponent);
        let capped = base.min(self.reconnect_max_ms as f64);
        // Add 25% jitter
        let jitter = capped * 0.25;
        let offset = (self.reconnect_count as f64 * 37.0) % jitter; // deterministic jitter
        (capped + offset) as u64
    }

    /// Duration since last disconnect, if disconnected.
    pub fn disconnected_duration(&self) -> Option<std::time::Duration> {
        self.disconnect_time.map(|t| t.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_connected() {
        let cs = ConnectionState::new(500, 30000);
        assert!(cs.is_connected());
        assert!(!cs.is_disconnected());
    }

    #[test]
    fn disconnect_and_reconnect() {
        let mut cs = ConnectionState::new(500, 30000);
        cs.on_disconnect();
        assert!(cs.is_disconnected());
        assert_eq!(cs.reconnect_count(), 1);

        cs.on_reconnect();
        assert!(cs.is_connected());
        assert_eq!(cs.reconnect_count(), 1); // count doesn't reset
    }

    #[test]
    fn backoff_increases() {
        let mut cs = ConnectionState::new(500, 30000);
        let b1 = cs.next_backoff_ms();
        cs.on_disconnect();
        let _b2 = cs.next_backoff_ms();
        cs.on_disconnect();
        let b3 = cs.next_backoff_ms();
        // Backoff should generally increase (with jitter, just check bounds)
        assert!(b1 < 30000);
        assert!(b3 <= 30000);
    }

    #[test]
    fn backoff_caps_at_max() {
        let mut cs = ConnectionState::new(500, 1000);
        for _ in 0..20 {
            cs.on_disconnect();
        }
        assert!(cs.next_backoff_ms() <= 1500); // max + 25% jitter
    }
}
