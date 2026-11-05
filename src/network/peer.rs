//! WebRTC peer construction and event plumbing.
//!
//! We use `webrtc` (webrtc-rs) 0.21. It provides ICE, STUN, DTLS and SCTP data
//! channels in pure Rust, so the Windows build needs only the Rust toolchain and
//! the MSVC linker. No TURN server is configured anywhere in this module.

use std::fmt;
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use log::{debug, info, warn};
use serde::Serialize;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::sleep;
use webrtc::data_channel::DataChannel;
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceCandidate, RTCIceConnectionState, RTCIceGatheringState, RTCIceServer,
    RTCIceTransportPolicy, RTCPeerConnectionIceEvent, RTCPeerConnectionState,
};
use webrtc::runtime::TokioRuntime;

use crate::network::diagnostics::CandidateSummary;

pub const DATA_CHANNEL_LABEL: &str = "akikazu-phase0";
pub const DEFAULT_STUN_SERVERS: &[&str] = &[
    "stun:stun.l.google.com:19302",
    "stun:stun1.l.google.com:19302",
];
pub const DEFAULT_BIND_ADDRS: &[&str] = &["0.0.0.0:0", "[::]:0"];

/// Do not accept a local SDP before this much time has passed: gives STUN a
/// chance to add server-reflexive candidates.
const MIN_GATHER_WAIT: Duration = Duration::from_secs(2);
/// Stop waiting once candidate gathering has been quiet for this long.
const GATHER_QUIET_PERIOD: Duration = Duration::from_millis(900);
const GATHER_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerState {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
    Unknown,
}

impl From<RTCPeerConnectionState> for PeerState {
    fn from(state: RTCPeerConnectionState) -> Self {
        match state {
            RTCPeerConnectionState::New => Self::New,
            RTCPeerConnectionState::Connecting => Self::Connecting,
            RTCPeerConnectionState::Connected => Self::Connected,
            RTCPeerConnectionState::Disconnected => Self::Disconnected,
            RTCPeerConnectionState::Failed => Self::Failed,
            RTCPeerConnectionState::Closed => Self::Closed,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for PeerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::New => f.write_str("new"),
            Self::Connecting => f.write_str("connecting"),
            Self::Connected => f.write_str("connected"),
            Self::Disconnected => f.write_str("disconnected"),
            Self::Failed => f.write_str("failed"),
            Self::Closed => f.write_str("closed"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub stun_servers: Vec<String>,
    pub bind_addrs: Vec<String>,
    pub gather_timeout: Duration,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            stun_servers: DEFAULT_STUN_SERVERS
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            bind_addrs: default_bind_addrs(),
            gather_timeout: Duration::from_secs(6),
        }
    }
}

/// Selects the local addresses the OS would use for the default route.
///
/// Binding `0.0.0.0`/`[::]` makes webrtc-rs open one socket per interface,
/// including Hyper-V, WSL and VPN adapters that cannot reach the peer. Those
/// extra sockets create noisy ICE checks and slow gathering. A UDP `connect`
/// performs only a route lookup (no packet is sent), so we bind the route's
/// source addresses instead. Users who need every interface can pass `--bind`.
pub fn default_bind_addrs() -> Vec<String> {
    let mut addresses = Vec::new();
    if let Some(address) = route_source_address("8.8.8.8:80", "0.0.0.0:0") {
        addresses.push(address);
    }
    if let Some(address) = route_source_address("[2001:4860:4860::8888]:80", "[::]:0") {
        addresses.push(address);
    }
    if addresses.is_empty() {
        DEFAULT_BIND_ADDRS
            .iter()
            .map(|value| (*value).to_string())
            .collect()
    } else {
        addresses
    }
}

fn route_source_address(remote: &str, bind: &str) -> Option<String> {
    let socket = UdpSocket::bind(bind).ok()?;
    socket.connect(remote).ok()?;
    let local = socket.local_addr().ok()?;
    if local.ip().is_unspecified() || local.ip().is_loopback() {
        return None;
    }
    Some(std::net::SocketAddr::new(local.ip(), 0).to_string())
}

pub struct Peer {
    pub pc: Arc<dyn PeerConnection>,
    pub channel_rx: UnboundedReceiver<Arc<dyn DataChannel>>,
    pub state_rx: UnboundedReceiver<PeerState>,
    pub started_at: Instant,
    gather_rx: UnboundedReceiver<()>,
    candidates: Arc<Mutex<CandidateSummary>>,
    gather_timeout: Duration,
}

impl Peer {
    pub async fn new(config: NetworkConfig) -> Result<Self> {
        let (channel_tx, channel_rx) = mpsc::unbounded_channel();
        let (state_tx, state_rx) = mpsc::unbounded_channel();
        let (gather_tx, gather_rx) = mpsc::unbounded_channel();
        let candidates = Arc::new(Mutex::new(CandidateSummary::default()));
        let sink = EventSink {
            channels: channel_tx,
            states: state_tx,
            gather: gather_tx,
            candidates: Arc::clone(&candidates),
        };

        let mut bind_addrs = config.bind_addrs.clone();
        if bind_addrs.is_empty() {
            bind_addrs = default_bind_addrs();
        }

        let started_at = Instant::now();
        let pc = match build_once(&config, bind_addrs.clone(), sink.clone()).await {
            Ok(pc) => Arc::new(pc) as Arc<dyn PeerConnection>,
            Err(first_error) if bind_addrs.len() > 1 => {
                warn!("dual-stack UDP bind failed ({first_error:#}); retrying with IPv4 only");
                let pc = build_once(&config, vec!["0.0.0.0:0".to_string()], sink)
                    .await
                    .context("build peer connection with IPv4 fallback")?;
                Arc::new(pc) as Arc<dyn PeerConnection>
            }
            Err(error) => return Err(error),
        };

        Ok(Self {
            pc,
            channel_rx,
            state_rx,
            started_at,
            gather_rx,
            candidates,
            gather_timeout: config.gather_timeout,
        })
    }

    pub fn candidate_summary(&self) -> CandidateSummary {
        self.candidates
            .lock()
            .map(|summary| summary.clone())
            .unwrap_or_default()
    }

    /// Waits for ICE gathering to finish, or for candidates to go quiet.
    ///
    /// webrtc-rs 0.21 does not always emit `Complete` when both IPv4 and IPv6
    /// wildcard sockets are used, so the quiet-period fallback is intentional.
    pub async fn wait_for_gathering(&mut self) -> Result<()> {
        let started = Instant::now();
        let mut last_change = started;
        let mut last_total = self.candidate_summary().total;

        loop {
            if self.gather_rx.try_recv().is_ok() {
                info!("ICE gathering complete");
                break;
            }
            if started.elapsed() >= self.gather_timeout {
                warn!(
                    "ICE gathering still running after {}s; using current candidates",
                    self.gather_timeout.as_secs()
                );
                break;
            }

            sleep(GATHER_POLL_INTERVAL).await;

            let total = self.candidate_summary().total;
            if total != last_total {
                last_total = total;
                last_change = Instant::now();
            }
            if started.elapsed() >= MIN_GATHER_WAIT && last_change.elapsed() >= GATHER_QUIET_PERIOD
            {
                debug!(
                    "ICE candidates have been quiet for {:?}",
                    GATHER_QUIET_PERIOD
                );
                break;
            }
        }

        if self.candidate_summary().total == 0 {
            bail!("no local ICE candidates were gathered");
        }
        Ok(())
    }

    pub async fn wait_for_channel(&mut self, timeout: Duration) -> Result<Arc<dyn DataChannel>> {
        tokio::time::timeout(timeout, self.channel_rx.recv())
            .await
            .context("timed out waiting for the peer to open a data channel")?
            .ok_or_else(|| anyhow!("peer connection closed before a data channel arrived"))
    }

    pub async fn create_data_channel(&self) -> Result<Arc<dyn DataChannel>> {
        self.pc
            .create_data_channel(DATA_CHANNEL_LABEL, None)
            .await
            .context("create data channel")
    }
}

#[derive(Clone)]
struct EventSink {
    channels: UnboundedSender<Arc<dyn DataChannel>>,
    states: UnboundedSender<PeerState>,
    gather: UnboundedSender<()>,
    candidates: Arc<Mutex<CandidateSummary>>,
}

struct EventHandler {
    sink: EventSink,
}

#[async_trait]
impl PeerConnectionEventHandler for EventHandler {
    async fn on_data_channel(&self, data_channel: Arc<dyn DataChannel>) {
        let _ = self.sink.channels.send(data_channel);
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        let state = PeerState::from(state);
        debug!("peer connection state: {state}");
        let _ = self.sink.states.send(state);
    }

    async fn on_ice_connection_state_change(&self, state: RTCIceConnectionState) {
        debug!("ICE connection state: {state:?}");
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        debug!("ICE gathering state: {state:?}");
        if state == RTCIceGatheringState::Complete {
            let _ = self.sink.gather.send(());
        }
    }

    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(mut summary) = self.sink.candidates.lock() {
            summary.observe(&event.candidate);
        }
        debug!(
            "local candidate: {:?} {}",
            event.candidate.typ,
            family_label(&event.candidate)
        );
    }
}

fn family_label(candidate: &RTCIceCandidate) -> &'static str {
    match candidate.address.parse::<std::net::IpAddr>() {
        Ok(address) if address.is_ipv6() => "IPv6",
        Ok(_) => "IPv4",
        Err(_) => "unknown-family",
    }
}

async fn build_once(
    config: &NetworkConfig,
    bind_addrs: Vec<String>,
    sink: EventSink,
) -> Result<impl PeerConnection> {
    let handlers = Arc::new(EventHandler { sink });
    let ice_servers = config
        .stun_servers
        .iter()
        .filter(|url| !url.trim().is_empty())
        .map(|url| RTCIceServer {
            urls: vec![url.clone()],
            ..Default::default()
        })
        .collect::<Vec<_>>();

    let configuration = RTCConfigurationBuilder::new()
        .with_ice_servers(ice_servers)
        .with_ice_transport_policy(RTCIceTransportPolicy::All)
        .build();

    PeerConnectionBuilder::new()
        .with_configuration(configuration)
        .with_handler(handlers)
        .with_runtime(Arc::new(TokioRuntime))
        .with_udp_addrs(bind_addrs.clone())
        .with_data_channel_send_buffer_limit(16 * 1024 * 1024)
        .build()
        .await
        .with_context(|| format!("bind UDP sockets at {}", bind_addrs.join(", ")))
}
