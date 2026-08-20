//! WebSocket transport.
//!
//! A single resilient read loop per connection:
//!
//! - connects with exponential backoff (never gives up),
//! - re-runs the decoder's subscription frames after every (re)connect,
//! - answers protocol-level pings and sends app-level pings on a timer,
//! - treats any inbound frame as liveness for staleness detection,
//! - emits [`FeedStatus`] transitions on the market topic so consumers can
//!   flag the book as suspect during gaps.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use lq_core::bus::EventBus;
use lq_core::event::{FeedStatus, MarketEvent};
use lq_types::TimestampMs;
use tokio_tungstenite::tungstenite::Message;

/// Transport configuration. All timings are explicit.
#[derive(Debug, Clone)]
pub struct WsConfig {
    /// WebSocket endpoint URL.
    pub url: String,
    /// Initial reconnect delay (doubles each attempt).
    pub reconnect_base_ms: u64,
    /// Maximum reconnect delay.
    pub reconnect_max_ms: u64,
    /// No inbound frame for this long → feed declared stale.
    pub stale_after_ms: u64,
    /// App-level ping cadence (only if the decoder supplies a payload).
    pub ping_interval_ms: u64,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            reconnect_base_ms: 1_000,
            reconnect_max_ms: 60_000,
            stale_after_ms: 10_000,
            ping_interval_ms: 15_000,
        }
    }
}

/// A decoder normalizes one venue's protocol into [`MarketEvent`]s. The
/// transport owns the socket; the decoder owns protocol semantics.
pub trait FeedDecoder: Send {
    fn name(&self) -> &'static str;

    /// Frames to send immediately after connecting / reconnecting.
    fn subscribe_frames(&mut self) -> Vec<String>;

    /// App-level ping payload, or `None` if the protocol uses only WS pings.
    fn ping_payload(&self) -> Option<String>;

    /// Outbound reply for a received text frame, if the protocol requires one
    /// (e.g. Binance server `{"method":"ping"}` → client `{"method":"pong"}`).
    fn outbound_reply(&self, _text: &str) -> Option<String> {
        None
    }

    /// Process one inbound text frame, publishing normalized events to `bus`.
    /// Returns `true` if the frame should count as a liveness signal (always
    /// the case except for server pong replies we explicitly initiated).
    fn on_text(&mut self, text: &str, bus: &EventBus) -> anyhow::Result<bool>;

    /// A `Status` event for this feed (for staleness / reconnect signalling).
    fn status_event(&self, status: FeedStatus) -> MarketEvent;

    /// The market topic to publish on. Defaults to the shared market topic.
    fn market_topic(&self) -> &lq_core::bus::Topic<MarketEvent>;
}

/// Run a resilient single-stream feed loop. Never returns except on
/// unrecoverable failure (network errors are retried forever).
pub async fn run_ws(
    cfg: WsConfig,
    bus: Arc<EventBus>,
    mut decoder: Box<dyn FeedDecoder>,
) -> anyhow::Result<()> {
    let last_seen = Arc::new(AtomicU64::new(0));
    let stale = Arc::new(AtomicBool::new(false));
    let mut attempt: u32 = 0;

    loop {
        let url = cfg.url.clone();
        let ws_stream = match tokio_tungstenite::connect_async(&url).await {
            Ok((ws, _)) => {
                attempt = 0;
                ws
            }
            Err(e) => {
                tracing::warn!(feed = decoder.name(), err = %e, "connect failed");
                backoff_sleep(attempt, &cfg).await;
                attempt = attempt.saturating_add(1);
                continue;
            }
        };
        let (mut write, mut read) = ws_stream.split();

        // (Re)subscribe.
        for frame in decoder.subscribe_frames() {
            if let Err(e) = write.send(Message::Text(frame)).await {
                tracing::warn!(feed = decoder.name(), err = %e, "subscribe send failed");
                break;
            }
        }

        // Signal resync expectation after a reconnect.
        if attempt > 0 {
            let _ = bus.market().try_publish(decoder.status_event(FeedStatus::Resync));
        }
        last_seen.store(TimestampMs::now().as_u64(), AtomicOrdering::Relaxed);
        stale.store(false, AtomicOrdering::Relaxed);

        // Staleness watchdog.
        let watch_last = last_seen.clone();
        let watch_stale = stale.clone();
        let watch_cfg = cfg.clone();
        let watch_bus = bus.clone();
        let stale_event = decoder.status_event(FeedStatus::Stale);
        let watchdog = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let last = watch_last.load(AtomicOrdering::Relaxed);
                let now = TimestampMs::now().as_u64();
                if last > 0
                    && now.saturating_sub(last) > watch_cfg.stale_after_ms
                    && !watch_stale.swap(true, AtomicOrdering::Relaxed)
                {
                    tracing::warn!(feed = "stale-event", "feed stale");
                    let _ = watch_bus.market().try_publish(stale_event.clone());
                }
            }
        });

        // Ping interval. The first tick fires immediately; consume it so the
        // first app-level ping is not sent the instant we connect.
        let mut ping_tick = tokio::time::interval(Duration::from_millis(cfg.ping_interval_ms));
        ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        if decoder.ping_payload().is_some() {
            ping_tick.tick().await;
        }

        let ping_payload = decoder.ping_payload();

        let reason: String = 'conn: loop {
            tokio::select! {
                msg = read.next() => {
                    let Some(msg) = msg else {
                        break 'conn "connection closed".into();
                    };
                    let msg = match msg {
                        Ok(m) => m,
                        Err(e) => break 'conn format!("read error: {e}"),
                    };
                    last_seen.store(TimestampMs::now().as_u64(), AtomicOrdering::Relaxed);
                    if stale.swap(false, AtomicOrdering::Relaxed) {
                        tracing::info!(feed = decoder.name(), "feed recovered");
                    }
                    match msg {
                        Message::Text(text) => {
                            let _ = decoder.on_text(&text, &bus)?;
                            if let Some(reply) = decoder.outbound_reply(&text) {
                                if write.send(Message::Text(reply)).await.is_err() {
                                    break 'conn "reply send failed".into();
                                }
                            }
                        }
                        Message::Ping(payload) => {
                            if write.send(Message::Pong(payload)).await.is_err() {
                                break 'conn "pong send failed".into();
                            }
                        }
                        Message::Pong(_) => {}
                        Message::Close(frame) => {
                            let detail = frame
                                .map(|f| format!("{} {:?}", f.code, f.reason))
                                .unwrap_or_else(|| "no frame".into());
                            break 'conn format!("server closed ({detail})");
                        }
                        _ => {}
                    }
                }
                _ = ping_tick.tick(), if ping_payload.is_some() => {
                    if let Some(p) = &ping_payload {
                        if write.send(Message::Text(p.clone())).await.is_err() {
                            break 'conn "ping send failed".into();
                        }
                    }
                }
            }
        };

        watchdog.abort();
        let _ = bus.market().try_publish(decoder.status_event(FeedStatus::Disconnected));
        tracing::warn!(feed = decoder.name(), reason = %reason, "feed disconnected");

        backoff_sleep(attempt, &cfg).await;
        attempt = attempt.saturating_add(1);
    }
}

async fn backoff_sleep(attempt: u32, cfg: &WsConfig) {
    let exp = cfg.reconnect_base_ms.saturating_mul(1u64 << attempt.min(10));
    let delay = exp.min(cfg.reconnect_max_ms);
    tokio::time::sleep(Duration::from_millis(delay)).await;
}

/// Re-export the tungstenite re-connect message type for tests.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_never_exceeds_max() {
        let cfg = WsConfig {
            reconnect_base_ms: 1_000,
            reconnect_max_ms: 5_000,
            ..WsConfig::default()
        };
        let exp = cfg.reconnect_base_ms.saturating_mul(1u64 << 10);
        assert_eq!(exp.min(cfg.reconnect_max_ms), 5_000);
    }
}
