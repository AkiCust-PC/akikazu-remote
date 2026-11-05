//! Phase 0 session: wait for the data channel, exchange Ping/Pong, and report.
//!
//! The data channel is reliable and ordered by default. Ping/Pong is an
//! application-level RTT probe on top of SCTP; ICE's own STUN RTT is shown
//! separately in the selected candidate pair when available.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use log::debug;
use serde::Serialize;
use tokio::sync::mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::peer_connection::PeerConnection;

use crate::network::diagnostics::{CandidateSummary, SelectedPath, detect_selected_path};
use crate::network::peer::{Peer, PeerState};
use crate::protocol::{
    PROTOCOL_VERSION, Role, RttSnapshot, RttTracker, WireMessage, duration_ms, unix_time_ms,
};

const DISCONNECT_GRACE: Duration = Duration::from_secs(15);
const STATS_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEnd {
    UserExit,
    DurationReached,
    PeerClosed,
    ConnectionFailed,
}

impl SessionEnd {
    pub fn is_success(self) -> bool {
        matches!(self, Self::UserExit | Self::DurationReached)
    }

    fn label(self) -> &'static str {
        match self {
            Self::UserExit => "user exit",
            Self::DurationReached => "duration reached",
            Self::PeerClosed => "peer closed",
            Self::ConnectionFailed => "connection failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
    pub connect_timeout: Duration,
    pub run_for: Option<Duration>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(1),
            ping_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(30),
            run_for: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionReport {
    pub schema_version: u32,
    pub role: String,
    pub result: SessionEnd,
    pub started_unix_s: u64,
    pub duration_ms: u64,
    pub connection_setup_ms: u64,
    pub peer_state: String,
    pub local_candidates: CandidateSummary,
    pub selected_path: Option<SelectedPath>,
    pub rtt: RttSnapshot,
    pub disconnect_count: u64,
    pub data_channel: String,
    pub notes: Vec<String>,
}

impl SessionReport {
    pub fn write_json(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create directory {}", parent.display()))?;
            }
        }
        let data = serde_json::to_vec_pretty(self).context("serialize diagnostics report")?;
        fs::write(path, data).with_context(|| format!("write {}", path.display()))
    }

    pub fn print_summary(&self) {
        println!();
        println!("--- Phase 0 summary ---");
        println!("Result: {}", self.result.label());
        println!("Peer state: {}", self.peer_state);
        match &self.selected_path {
            Some(path) => println!("Path: {}", path.label()),
            None => println!("Path: not available (no nominated candidate pair in stats)"),
        }
        println!(
            "RTT: min {} ms / avg {} ms / max {} ms",
            format_opt(self.rtt.min_ms),
            format_opt(self.rtt.avg_ms),
            format_opt(self.rtt.max_ms)
        );
        println!(
            "Ping loss: {:.1}% ({} lost / {} received, {} still pending)",
            self.rtt.loss_rate * 100.0,
            self.rtt.lost,
            self.rtt.received,
            self.rtt.pending
        );
        println!("Disconnects: {}", self.disconnect_count);
        println!("ICE candidates: {}", self.local_candidates.total);
    }
}

fn format_opt(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1}"))
        .unwrap_or_else(|| "n/a".to_string())
}

enum ChannelEvent {
    Text(String),
    Binary(usize),
    Closed,
    Error,
}

pub async fn run(
    peer: Peer,
    data_channel: Arc<dyn DataChannel>,
    role: Role,
    config: SessionConfig,
) -> Result<SessionReport> {
    let pc: Arc<dyn PeerConnection> = Arc::clone(&peer.pc);
    let started_at = peer.started_at;
    let started_unix_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let local_candidates = peer.candidate_summary();
    let mut state_rx = peer.state_rx;

    wait_until_open(&data_channel, &mut state_rx, config.connect_timeout).await?;
    let opened_at = std::time::Instant::now();
    println!("P2P status: connected");
    println!(
        "Connection setup: {} ms",
        millis_u64(opened_at.duration_since(started_at))
    );

    let mut notes = vec![
        "TURN is not configured; relay candidates are not expected.".to_string(),
        "IPv4/IPv6 availability is based on local ICE candidates, not peer reachability."
            .to_string(),
    ];

    let selected_path = wait_for_path(&pc).await;
    match &selected_path {
        Some(path) => println!("Connection path: {}", path.label()),
        None => println!("Connection path: not available yet (will retry at exit)"),
    }
    println!("STUN: {}", local_candidates.stun_status());

    if let Err(error) = data_channel
        .send_text(
            &WireMessage::Hello {
                protocol: PROTOCOL_VERSION,
                role,
            }
            .encode()?,
        )
        .await
    {
        notes.push(format!("failed to send hello: {error}"));
    }

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    spawn_channel_reader(Arc::clone(&data_channel), event_tx);

    let mut rtt = RttTracker::default();
    let mut ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + config.ping_interval,
        config.ping_interval,
    );
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let session_started = std::time::Instant::now();
    let end: SessionEnd;
    let mut last_state = PeerState::Connected;
    let mut final_state = PeerState::Connected;
    let mut disconnect_count = 0_u64;
    let mut disconnected_at: Option<std::time::Instant> = None;
    let mut seq = 0_u64;
    let mut binary_messages = 0_u64;

    loop {
        tokio::select! {
            biased;

            _ = &mut ctrl_c => {
                end = SessionEnd::UserExit;
                break;
            }

            _ = ticker.tick() => {
                if let Some(limit) = config.run_for {
                    if session_started.elapsed() >= limit {
                        end = SessionEnd::DurationReached;
                        break;
                    }
                }
                if let Some(since) = disconnected_at {
                    if since.elapsed() >= DISCONNECT_GRACE {
                        notes.push(format!(
                            "peer stayed disconnected for more than {}s",
                            DISCONNECT_GRACE.as_secs()
                        ));
                        end = SessionEnd::ConnectionFailed;
                        break;
                    }
                }

                let expired = rtt.expire(std::time::Instant::now(), config.ping_timeout);
                if expired > 0 {
                    println!("Ping timeout: {expired} packet(s)");
                }

                seq = seq.wrapping_add(1);
                let sent_at = std::time::Instant::now();
                let message = WireMessage::Ping {
                    seq,
                    sent_unix_ms: unix_time_ms(),
                };
                match data_channel.send_text(&message.encode()?).await {
                    Ok(()) => rtt.on_ping_sent(seq, sent_at),
                    Err(error) => {
                        notes.push(format!("failed to send ping: {error}"));
                        end = SessionEnd::PeerClosed;
                        break;
                    }
                }
            }

            Some(event) = event_rx.recv() => {
                match event {
                    ChannelEvent::Text(text) => {
                        let message = match WireMessage::decode(&text) {
                            Ok(message) => message,
                            Err(error) => {
                                notes.push(format!("invalid peer message: {error}"));
                                continue;
                            }
                        };
                        match message {
                            WireMessage::Hello { protocol, role: peer_role } => {
                                if protocol != PROTOCOL_VERSION {
                                    notes.push(format!(
                                        "protocol mismatch: local {PROTOCOL_VERSION}, peer {protocol}"
                                    ));
                                    println!("Protocol mismatch with peer");
                                    end = SessionEnd::ConnectionFailed;
                                    break;
                                }
                                println!("Peer role: {peer_role}");
                            }
                            WireMessage::Ping { seq, sent_unix_ms } => {
                                let pong = WireMessage::Pong { seq, sent_unix_ms };
                                if let Err(error) = data_channel.send_text(&pong.encode()?).await {
                                    notes.push(format!("failed to send pong: {error}"));
                                    end = SessionEnd::PeerClosed;
                                    break;
                                }
                            }
                            WireMessage::Pong { seq, .. } => {
                                if let Some(rtt) = rtt.on_pong(seq, std::time::Instant::now()) {
                                    println!("RTT {:.1} ms (seq {seq})", duration_ms(rtt));
                                }
                            }
                            WireMessage::Bye { reason } => {
                                println!("Peer closed the session: {reason}");
                                end = SessionEnd::PeerClosed;
                                break;
                            }
                        }
                    }
                    ChannelEvent::Binary(size) => {
                        binary_messages += 1;
                        debug!("ignored binary message of {size} bytes");
                    }
                    ChannelEvent::Closed => {
                        end = SessionEnd::PeerClosed;
                        break;
                    }
                    ChannelEvent::Error => {
                        notes.push("data channel reported an error".to_string());
                    }
                }
            }

            Some(state) = state_rx.recv() => {
                if state != last_state {
                    println!("Peer state: {state}");
                }
                last_state = state;
                final_state = state;
                match state {
                    PeerState::Connected => {
                        disconnected_at = None;
                    }
                    PeerState::Disconnected => {
                        if disconnected_at.is_none() {
                            disconnect_count += 1;
                            disconnected_at = Some(std::time::Instant::now());
                        }
                    }
                    PeerState::Failed | PeerState::Closed => {
                        end = SessionEnd::ConnectionFailed;
                        break;
                    }
                    _ => {}
                }
            }

            else => {
                end = SessionEnd::PeerClosed;
                break;
            }
        }
    }

    if binary_messages > 0 {
        notes.push(format!("ignored {binary_messages} binary message(s)"));
    }

    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        data_channel.send_text(
            &WireMessage::Bye {
                reason: end.label().to_string(),
            }
            .encode()?,
        ),
    )
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(2), data_channel.close()).await;
    let _ = tokio::time::timeout(Duration::from_secs(3), pc.close()).await;

    let selected_path = match selected_path {
        Some(path) => Some(path),
        None => detect_selected_path(&pc).await,
    };

    let report = SessionReport {
        schema_version: 1,
        role: role.to_string(),
        result: end,
        started_unix_s,
        duration_ms: millis_u64(started_at.elapsed()),
        connection_setup_ms: millis_u64(opened_at.duration_since(started_at)),
        peer_state: final_state.to_string(),
        local_candidates,
        selected_path,
        rtt: rtt.snapshot(),
        disconnect_count,
        data_channel: "reliable/ordered".to_string(),
        notes,
    };
    Ok(report)
}

async fn wait_until_open(
    data_channel: &Arc<dyn DataChannel>,
    state_rx: &mut mpsc::UnboundedReceiver<PeerState>,
    timeout: Duration,
) -> Result<()> {
    let wait = async {
        loop {
            tokio::select! {
                event = data_channel.poll() => {
                    match event {
                        Some(DataChannelEvent::OnOpen) => return Ok(()),
                        Some(DataChannelEvent::OnClose) | None => {
                            bail!("data channel closed before it opened")
                        }
                        _ => {}
                    }
                }
                Some(state) = state_rx.recv() => {
                    println!("Peer state: {state}");
                    if matches!(state, PeerState::Failed | PeerState::Closed) {
                        bail!("peer connection failed before the data channel opened")
                    }
                }
            }
        }
    };

    tokio::time::timeout(timeout, wait).await.with_context(|| {
        format!(
            "timed out after {}s waiting for the P2P data channel",
            timeout.as_secs()
        )
    })?
}

async fn wait_for_path(pc: &Arc<dyn PeerConnection>) -> Option<SelectedPath> {
    let started = std::time::Instant::now();
    loop {
        if let Some(path) = detect_selected_path(pc).await {
            return Some(path);
        }
        if started.elapsed() >= STATS_WAIT {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn spawn_channel_reader(
    data_channel: Arc<dyn DataChannel>,
    event_tx: mpsc::UnboundedSender<ChannelEvent>,
) {
    tokio::spawn(async move {
        let mut sent_close = false;
        while let Some(event) = data_channel.poll().await {
            let message = match event {
                DataChannelEvent::OnMessage(message) => {
                    if message.is_string {
                        ChannelEvent::Text(String::from_utf8_lossy(&message.data).into_owned())
                    } else {
                        ChannelEvent::Binary(message.data.len())
                    }
                }
                DataChannelEvent::OnClose => {
                    sent_close = true;
                    ChannelEvent::Closed
                }
                DataChannelEvent::OnError => ChannelEvent::Error,
                _ => continue,
            };
            let is_close = matches!(message, ChannelEvent::Closed);
            if event_tx.send(message).is_err() {
                return;
            }
            if is_close {
                return;
            }
        }
        if !sent_close {
            let _ = event_tx.send(ChannelEvent::Closed);
        }
    });
}

fn millis_u64(value: Duration) -> u64 {
    value.as_millis().min(u128::from(u64::MAX)) as u64
}
