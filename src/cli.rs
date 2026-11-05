use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgAction, Args, Parser, Subcommand};

use crate::network::peer::{DEFAULT_STUN_SERVERS, NetworkConfig, default_bind_addrs};
use crate::session::SessionConfig;

#[derive(Debug, Parser)]
#[command(
    name = "akikazu-remote",
    version,
    about = "Akikazu Remote Phase 0: manual-signaling P2P connectivity test",
    long_about = "Builds an encrypted WebRTC DataChannel between two Windows PCs, then measures \
                  Ping/Pong RTT. Phase 0 uses manual SDP exchange and no TURN relay."
)]
pub struct Cli {
    /// Increase log verbosity (repeatable).
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create an offer and wait for the answer token.
    Offer(ExchangeArgs),
    /// Read an offer token, create an answer, and wait for the peer.
    Answer(ExchangeArgs),
}

#[derive(Debug, Args)]
pub struct ExchangeArgs {
    /// Offer side: write the offer token here. Answer side: read the offer token here.
    /// If omitted, the token is printed to stdout / read from stdin.
    #[arg(long, value_name = "PATH")]
    pub offer_file: Option<PathBuf>,

    /// Offer side: read the answer token here. Answer side: write the answer token here.
    /// If omitted, the token is read from stdin / printed to stdout.
    #[arg(long, value_name = "PATH")]
    pub answer_file: Option<PathBuf>,

    /// STUN server URI. Can be repeated. Defaults to two public STUN servers.
    #[arg(long = "stun", value_name = "URL", action = ArgAction::Append, conflicts_with = "no_stun")]
    pub stun: Vec<String>,

    /// Disable STUN entirely (host candidates only; useful for LAN tests).
    #[arg(long, conflicts_with = "stun")]
    pub no_stun: bool,

    /// Local UDP bind address. Can be repeated. Defaults to 0.0.0.0:0 and [::]:0.
    #[arg(long = "bind", value_name = "ADDR", action = ArgAction::Append)]
    pub bind: Vec<String>,

    /// Maximum time to wait for ICE candidate gathering.
    #[arg(long, default_value_t = 6)]
    pub gather_timeout_secs: u64,

    /// How long to wait for the operator to provide the other token.
    #[arg(long, default_value_t = 120)]
    pub exchange_timeout_secs: u64,

    /// How long to wait for the data channel after SDP exchange.
    #[arg(long, default_value_t = 30)]
    pub connect_timeout_secs: u64,

    /// Interval between application Ping messages.
    #[arg(long, default_value_t = 1)]
    pub ping_interval_secs: u64,

    /// A Ping with no Pong after this long is counted as lost.
    #[arg(long, default_value_t = 5)]
    pub ping_timeout_secs: u64,

    /// Stop after N seconds. If omitted, run until Ctrl-C.
    #[arg(long, value_name = "N")]
    pub duration_secs: Option<u64>,

    /// Write the JSON diagnostics report here.
    #[arg(
        long,
        default_value = "akikazu-phase0-report.json",
        value_name = "PATH"
    )]
    pub report: PathBuf,
}

impl ExchangeArgs {
    pub fn network_config(&self) -> NetworkConfig {
        let stun_servers = if self.no_stun {
            Vec::new()
        } else if self.stun.is_empty() {
            DEFAULT_STUN_SERVERS
                .iter()
                .map(|value| (*value).to_string())
                .collect()
        } else {
            self.stun.clone()
        };
        let bind_addrs = if self.bind.is_empty() {
            default_bind_addrs()
        } else {
            self.bind.clone()
        };

        NetworkConfig {
            stun_servers,
            bind_addrs,
            gather_timeout: Duration::from_secs(self.gather_timeout_secs.max(1)),
        }
    }

    pub fn session_config(&self) -> SessionConfig {
        SessionConfig {
            ping_interval: Duration::from_secs(self.ping_interval_secs.max(1)),
            ping_timeout: Duration::from_secs(self.ping_timeout_secs.max(1)),
            connect_timeout: Duration::from_secs(self.connect_timeout_secs.max(1)),
            run_for: self
                .duration_secs
                .filter(|seconds| *seconds > 0)
                .map(Duration::from_secs),
        }
    }

    pub fn exchange_timeout(&self) -> Duration {
        Duration::from_secs(self.exchange_timeout_secs.max(1))
    }
}
