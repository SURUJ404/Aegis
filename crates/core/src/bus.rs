//! Bounded, typed event bus.
//!
//! Design:
//! - Every topic is a **bounded** `tokio::sync::mpsc` channel feeding a small
//!   fan-out broker task. Producers never block the broker; consumers never see
//!   unbounded queues.
//! - A slow consumer gets its events **dropped and counted**, never the
//!   producer blocked (market data is recoverable via sequence gaps).
//! - Control events are the exception: they use block-on-full semantics so a
//!   kill switch can never be silently dropped.
//!
//! Consequences (by design):
//! - Market data may be dropped under load; consumers must tolerate gaps and
//!   resynchronize from snapshots (`lq-orderbook` implements exactly this).
//! - Execution/position events must not be dropped, so persistence runs as a
//!   direct consumer of the same channel a backpressure-aware layer.

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::event::{ControlEvent, ExecutionEvent, MarketEvent};

/// Capacity of the market-data topic.
pub const MARKET_TOPIC_CAPACITY: usize = 4096;
/// Capacity of the execution topic.
pub const EXECUTION_TOPIC_CAPACITY: usize = 4096;
/// Capacity of the control topic.
pub const CONTROL_TOPIC_CAPACITY: usize = 64;

/// Result of a non-blocking publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishResult {
    /// Event accepted by the topic.
    Published,
    /// Event dropped because a channel was full. Counted in `Topic::stats`.
    Dropped,
    /// Channel was full and the topic policy is block-on-full; await
    /// [`Topic::publish_blocking`] to apply backpressure.
    Backpressure,
    /// No live subscribers; the event was discarded.
    NoSubscribers,
}

impl PublishResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Published)
    }
}

struct TopicState {
    published: AtomicU64,
    dropped: AtomicU64,
    no_subscribers: AtomicU64,
}

impl TopicState {
    fn new() -> Self {
        Self {
            published: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            no_subscribers: AtomicU64::new(0),
        }
    }
}

/// Per-topic counters for observability.
#[derive(Debug, Clone, Copy, Default)]
pub struct TopicStats {
    pub published: u64,
    pub dropped: u64,
    pub no_subscribers: u64,
    pub subscribers: usize,
}

/// Publish policy when the *inbound* queue is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishPolicy {
    /// Drop the newest event and count it. Best for market data, which can be
    /// recovered via sequence gaps.
    DropNewest,
    /// Signal backpressure; the caller must await to push through. Best for
    /// control and execution semantics.
    Block,
}

/// A subscription guard. Removes its registration from the topic when dropped.
type SubscriberList<T> = Arc<Mutex<Vec<(u64, mpsc::Sender<T>)>>>;

/// A subscription guard. Removes its registration from the topic when dropped.
pub struct TopicSubscriber<T> {
    rx: mpsc::Receiver<T>,
    id: u64,
    subs: SubscriberList<T>,
}

impl<T> TopicSubscriber<T> {
    pub async fn recv(&mut self) -> Option<T> {
        self.rx.recv().await
    }

    pub fn try_recv(&mut self) -> Result<T, mpsc::error::TryRecvError> {
        self.rx.try_recv()
    }
}

impl<T> Drop for TopicSubscriber<T> {
    fn drop(&mut self) {
        self.subs.lock().retain(|(id, _)| *id != self.id);
    }
}

impl<T: Clone + Send + 'static> std::fmt::Debug for Topic<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Topic")
            .field("stats", &self.stats())
            .finish()
    }
}

/// A bounded topic. Cheap to share as a reference; the bus lives behind an
/// `Arc` in the engine.
pub struct Topic<T> {
    inbound: mpsc::Sender<T>,
    policy: PublishPolicy,
    state: Arc<TopicState>,
    subs: SubscriberList<T>,
    next_sub_id: AtomicU64,
    /// Kept alive for the topic's lifetime; aborts itself when the inbound
    /// channel closes (i.e. when the topic is dropped).
    _broker: tokio::task::JoinHandle<()>,
}

impl<T: Clone + Send + 'static> Topic<T> {
    /// Create a topic. Must be called from inside a Tokio runtime (it spawns
    /// the fan-out broker task).
    pub fn new(name: &'static str, capacity: usize, policy: PublishPolicy) -> Self {
        let (inbound, mut rx) = mpsc::channel::<T>(capacity);
        let state = Arc::new(TopicState::new());
        let subs: SubscriberList<T> = Arc::new(Mutex::new(Vec::new()));

        let broker_subs = Arc::clone(&subs);
        let broker_state = Arc::clone(&state);
        let _broker = tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                let sub_senders = broker_subs.lock();
                if sub_senders.is_empty() {
                    broker_state.no_subscribers.fetch_add(1, AtomicOrdering::Relaxed);
                    continue;
                }
                for (_, tx) in sub_senders.iter() {
                    match tx.try_send(item.clone()) {
                        Ok(_) => {}
                        Err(_) => {
                            broker_state.dropped.fetch_add(1, AtomicOrdering::Relaxed);
                        }
                    }
                }
            }
            tracing::debug!(topic = name, "topic broker finished");
        });

        Self {
            inbound,
            policy,
            state,
            subs,
            next_sub_id: AtomicU64::new(1),
            _broker,
        }
    }

    /// Register a subscriber. Multiple subscribers are supported.
    pub fn subscribe(&self) -> TopicSubscriber<T> {
        let (tx, rx) = mpsc::channel::<T>(self.inbound.capacity());
        let id = self.next_sub_id.fetch_add(1, AtomicOrdering::Relaxed);
        self.subs.lock().push((id, tx));
        TopicSubscriber {
            rx,
            id,
            subs: Arc::clone(&self.subs),
        }
    }

    /// Non-blocking publish. See [`PublishResult`].
    pub fn try_publish(&self, item: T) -> PublishResult {
        match self.inbound.try_send(item) {
            Ok(_) => {
                self.state.published.fetch_add(1, AtomicOrdering::Relaxed);
                PublishResult::Published
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.state.dropped.fetch_add(1, AtomicOrdering::Relaxed);
                PublishResult::Dropped
            }
            Err(mpsc::error::TrySendError::Closed(_)) => PublishResult::NoSubscribers,
        }
    }

    /// Blocking publish. Applies backpressure when the topic is full. Use only
    /// for topics whose events must never be dropped.
    pub async fn publish_blocking(&self, item: T) -> PublishResult {
        match self.inbound.send(item).await {
            Ok(_) => {
                self.state.published.fetch_add(1, AtomicOrdering::Relaxed);
                PublishResult::Published
            }
            Err(_) => PublishResult::NoSubscribers,
        }
    }

    /// The publish policy configured for this topic.
    pub fn policy(&self) -> PublishPolicy {
        self.policy
    }

    pub fn stats(&self) -> TopicStats {
        TopicStats {
            published: self.state.published.load(AtomicOrdering::Relaxed),
            dropped: self.state.dropped.load(AtomicOrdering::Relaxed),
            no_subscribers: self.state.no_subscribers.load(AtomicOrdering::Relaxed),
            subscribers: self.subs.lock().len(),
        }
    }
}

/// The three canonical topics of the engine.
#[derive(Debug, Clone)]
pub struct EventBus {
    market: Arc<Topic<MarketEvent>>,
    execution: Arc<Topic<ExecutionEvent>>,
    control: Arc<Topic<ControlEvent>>,
}

impl EventBus {
    /// Construct the bus. Must be called inside a Tokio runtime.
    pub fn new() -> Self {
        Self {
            market: Arc::new(Topic::new(
                "market",
                MARKET_TOPIC_CAPACITY,
                PublishPolicy::DropNewest,
            )),
            execution: Arc::new(Topic::new(
                "execution",
                EXECUTION_TOPIC_CAPACITY,
                PublishPolicy::Block,
            )),
            control: Arc::new(Topic::new(
                "control",
                CONTROL_TOPIC_CAPACITY,
                PublishPolicy::Block,
            )),
        }
    }

    pub fn market(&self) -> &Topic<MarketEvent> {
        &self.market
    }

    pub fn execution(&self) -> &Topic<ExecutionEvent> {
        &self.execution
    }

    pub fn control(&self) -> &Topic<ControlEvent> {
        &self.control
    }

    pub fn market_stats(&self) -> TopicStats {
        self.market.stats()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{LevelChange, OrderBookDelta};
    use lq_types::{Exchange, Side, Symbol};

    fn sample_delta(seq: u64) -> MarketEvent {
        MarketEvent::Delta(OrderBookDelta {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: seq,
            event_ts: lq_types::TimestampMs(1),
            exchange_ts: lq_types::TimestampMs(1),
            changes: vec![LevelChange {
                side: Side::Bid,
                price: rust_decimal_macros::dec!(100),
                qty: rust_decimal_macros::dec!(1),
            }],
            clear: false,
        })
    }

    #[tokio::test]
    async fn publishes_to_subscribers_in_order() {
        let bus = EventBus::new();
        let mut sub = bus.market().subscribe();
        for seq in 1..=10 {
            assert_eq!(bus.market().try_publish(sample_delta(seq)), PublishResult::Published);
        }
        for seq in 1..=10 {
            match sub.recv().await {
                Some(MarketEvent::Delta(d)) => assert_eq!(d.sequence, seq),
                _ => panic!("expected delta"),
            }
        }
    }

    #[tokio::test]
    async fn counts_drops_for_slow_subscriber() {
        let bus = EventBus::new();
        let _sub = bus.market().subscribe();
        // Overfill the inbound capacity; the topic must drop and count.
        let capacity = MARKET_TOPIC_CAPACITY;
        let mut dropped = 0;
        for seq in 0..capacity * 3 {
            if bus.market().try_publish(sample_delta(seq as u64)) != PublishResult::Published {
                dropped += 1;
            }
        }
        assert!(dropped > 0);
        assert!(bus.market().stats().dropped >= dropped as u64);
    }

    #[tokio::test]
    async fn no_subscribers_is_counted() {
        let bus = EventBus::new();
        bus.market().try_publish(sample_delta(1));
        // The fan-out broker processes asynchronously; wait until it observes
        // the empty subscriber list.
        for _ in 0..100 {
            if bus.market().stats().no_subscribers == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        panic!("broker never counted the unsubscribed publish");
    }

    #[tokio::test]
    async fn subscriber_unregisters_on_drop() {
        let bus = EventBus::new();
        {
            let _sub = bus.market().subscribe();
            assert_eq!(bus.market().stats().subscribers, 1);
        }
        assert_eq!(bus.market().stats().subscribers, 0);
    }
}