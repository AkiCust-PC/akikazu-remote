//! Wire protocol for the Phase 0 data channel.
//!
//! Everything sent here travels inside the WebRTC DataChannel, which is already
//! encrypted by DTLS/SCTP. This layer is deliberately small: it only proves the
//! channel works and measures latency.

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Offerer,
    Answerer,
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offerer => f.write_str("offerer"),
            Self::Answerer => f.write_str("answerer"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireMessage {
    Hello { protocol: u16, role: Role },
    Ping { seq: u64, sent_unix_ms: u64 },
    Pong { seq: u64, sent_unix_ms: u64 },
    Bye { reason: String },
}

impl WireMessage {
    pub fn encode(&self) -> Result<String> {
        serde_json::to_string(self).context("encode wire message")
    }

    pub fn decode(text: &str) -> Result<Self> {
        serde_json::from_str(text).context("decode wire message")
    }
}

pub fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RttSnapshot {
    pub sent: u64,
    pub received: u64,
    pub lost: u64,
    pub pending: usize,
    pub min_ms: Option<f64>,
    pub avg_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub last_ms: Option<f64>,
    pub loss_rate: f64,
}

#[derive(Debug, Default)]
pub struct RttTracker {
    sent: u64,
    received: u64,
    lost: u64,
    min: Option<Duration>,
    max: Option<Duration>,
    total: Duration,
    last: Option<Duration>,
    pending: HashMap<u64, Instant>,
}

impl RttTracker {
    pub fn on_ping_sent(&mut self, seq: u64, now: Instant) {
        self.sent += 1;
        self.pending.insert(seq, now);
    }

    pub fn on_pong(&mut self, seq: u64, now: Instant) -> Option<Duration> {
        let sent_at = self.pending.remove(&seq)?;
        let rtt = now.saturating_duration_since(sent_at);
        self.received += 1;
        self.total += rtt;
        self.last = Some(rtt);
        self.min = Some(self.min.map_or(rtt, |value| value.min(rtt)));
        self.max = Some(self.max.map_or(rtt, |value| value.max(rtt)));
        Some(rtt)
    }

    /// Drops pings that have been outstanding longer than `timeout`.
    pub fn expire(&mut self, now: Instant, timeout: Duration) -> u64 {
        let before = self.pending.len();
        self.pending
            .retain(|_, sent_at| now.saturating_duration_since(*sent_at) < timeout);
        let expired = (before - self.pending.len()) as u64;
        self.lost += expired;
        expired
    }

    pub fn snapshot(&self) -> RttSnapshot {
        let avg = if self.received == 0 {
            None
        } else {
            Some(self.total.as_secs_f64() * 1000.0 / self.received as f64)
        };
        let denominator = self.received + self.lost;
        RttSnapshot {
            sent: self.sent,
            received: self.received,
            lost: self.lost,
            pending: self.pending.len(),
            min_ms: self.min.map(duration_ms),
            avg_ms: avg,
            max_ms: self.max.map(duration_ms),
            last_ms: self.last.map(duration_ms),
            loss_rate: if denominator == 0 {
                0.0
            } else {
                self.lost as f64 / denominator as f64
            },
        }
    }
}

pub fn duration_ms(value: Duration) -> f64 {
    value.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_message_roundtrip() {
        let message = WireMessage::Ping {
            seq: 42,
            sent_unix_ms: 1234,
        };
        let encoded = message.encode().expect("encode");
        assert_eq!(WireMessage::decode(&encoded).expect("decode"), message);
    }

    #[test]
    fn rtt_tracker_records_min_avg_max() {
        let mut tracker = RttTracker::default();
        let base = Instant::now();
        tracker.on_ping_sent(1, base);
        tracker.on_ping_sent(2, base);
        tracker.on_ping_sent(3, base);

        assert_eq!(
            tracker.on_pong(1, base + Duration::from_millis(10)),
            Some(Duration::from_millis(10))
        );
        assert_eq!(
            tracker.on_pong(2, base + Duration::from_millis(30)),
            Some(Duration::from_millis(30))
        );
        // Duplicate or unknown pongs are ignored.
        assert_eq!(tracker.on_pong(2, base + Duration::from_millis(40)), None);

        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.sent, 3);
        assert_eq!(snapshot.received, 2);
        assert_eq!(snapshot.lost, 0);
        assert_eq!(snapshot.pending, 1);
        assert_eq!(snapshot.min_ms, Some(10.0));
        assert_eq!(snapshot.avg_ms, Some(20.0));
        assert_eq!(snapshot.max_ms, Some(30.0));
        assert_eq!(snapshot.last_ms, Some(30.0));
        assert_eq!(snapshot.loss_rate, 0.0);
    }

    #[test]
    fn rtt_tracker_expires_lost_pings() {
        let base = Instant::now();
        let mut tracker = RttTracker::default();
        tracker.on_ping_sent(7, base);
        assert_eq!(
            tracker.expire(base + Duration::from_secs(6), Duration::from_secs(5)),
            1
        );
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.lost, 1);
        assert_eq!(snapshot.loss_rate, 1.0);
        assert_eq!(snapshot.pending, 0);
    }
}
