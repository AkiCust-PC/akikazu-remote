//! Diagnostics that can be produced without a signaling server.
//!
//! Privacy rule: the report records candidate families and types, never public
//! IP addresses. Raw addresses are only available behind `--show-addresses` on
//! the console, and are never written to the JSON report.

use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use serde::Serialize;
use webrtc::peer_connection::{
    PeerConnection, RTCIceCandidate, RTCStatsReportEntry, StatsSelector,
};

#[derive(Debug, Clone, Default, Serialize)]
pub struct CandidateSummary {
    pub total: usize,
    pub host_ipv4: usize,
    pub host_ipv6: usize,
    pub srflx_ipv4: usize,
    pub srflx_ipv6: usize,
    pub prflx: usize,
    pub relay: usize,
    pub other: usize,
}

impl CandidateSummary {
    pub fn observe(&mut self, candidate: &RTCIceCandidate) {
        self.total += 1;
        let family = candidate.address.parse::<IpAddr>().ok();
        let ipv6 = family.is_some_and(|address| address.is_ipv6());
        match format!("{:?}", candidate.typ).to_ascii_lowercase().as_str() {
            "host" if ipv6 => self.host_ipv6 += 1,
            "host" => self.host_ipv4 += 1,
            "srflx" if ipv6 => self.srflx_ipv6 += 1,
            "srflx" => self.srflx_ipv4 += 1,
            "prflx" => self.prflx += 1,
            "relay" => self.relay += 1,
            _ => self.other += 1,
        }
    }

    pub fn has_ipv4(&self) -> bool {
        self.host_ipv4 + self.srflx_ipv4 > 0
    }

    pub fn has_ipv6(&self) -> bool {
        self.host_ipv6 + self.srflx_ipv6 > 0
    }

    pub fn stun_status(&self) -> &'static str {
        if self.srflx_ipv4 + self.srflx_ipv6 > 0 {
            "reachable (server-reflexive candidate gathered)"
        } else {
            "unverified (no server-reflexive candidate)"
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SelectedPath {
    /// `ipv4`, `ipv6`, or `unknown`. This is measured from the nominated ICE
    /// candidate pair, not guessed from local interface state.
    pub family: String,
    pub local_candidate_type: String,
    pub remote_candidate_type: String,
    pub ice_rtt_ms: Option<f64>,
    pub packets_sent: u64,
    pub packets_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl SelectedPath {
    pub fn label(&self) -> String {
        let family = match self.family.as_str() {
            "ipv6" => "IPv6",
            "ipv4" => "IPv4",
            _ => "unknown family",
        };
        format!(
            "{} / {} candidate",
            family,
            if self.local_candidate_type.is_empty() {
                "unknown"
            } else {
                &self.local_candidate_type
            }
        )
    }
}

pub async fn detect_selected_path(pc: &Arc<dyn PeerConnection>) -> Option<SelectedPath> {
    let report = pc
        .get_stats(std::time::Instant::now(), StatsSelector::None)
        .await;

    let nominated = report
        .candidate_pairs()
        .filter(|pair| pair.nominated)
        .max_by_key(|pair| pair.packets_sent);
    let pair = nominated.or_else(|| {
        report
            .candidate_pairs()
            .max_by_key(|pair| pair.packets_sent)
    })?;

    let local = report.iter().find_map(|entry| match entry {
        RTCStatsReportEntry::LocalCandidate(candidate)
            if candidate.stats.id.ends_with(&pair.local_candidate_id) =>
        {
            Some(candidate)
        }
        _ => None,
    });
    let local_type = local.map(|candidate| {
        (
            candidate
                .address
                .as_deref()
                .and_then(|value| IpAddr::from_str(value).ok()),
            format!("{:?}", candidate.candidate_type).to_ascii_lowercase(),
        )
    });
    let remote_type = report
        .iter()
        .find_map(|entry| match entry {
            RTCStatsReportEntry::RemoteCandidate(candidate)
                if candidate.stats.id.ends_with(&pair.remote_candidate_id) =>
            {
                Some(format!("{:?}", candidate.candidate_type).to_ascii_lowercase())
            }
            _ => None,
        })
        .unwrap_or_else(|| "unknown".to_string());

    let (address, local_candidate_type) = local_type.unwrap_or((None, "unknown".to_string()));
    let family = match address {
        Some(address) if address.is_ipv6() => "ipv6",
        Some(_) => "ipv4",
        None => "unknown",
    };

    Some(SelectedPath {
        family: family.to_string(),
        local_candidate_type,
        remote_candidate_type: remote_type,
        ice_rtt_ms: (pair.current_round_trip_time > 0.0)
            .then_some(pair.current_round_trip_time * 1000.0),
        packets_sent: pair.packets_sent,
        packets_received: pair.packets_received,
        bytes_sent: pair.bytes_sent,
        bytes_received: pair.bytes_received,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_summary_is_not_reported_as_working() {
        let summary = CandidateSummary::default();
        assert!(!summary.has_ipv4());
        assert!(!summary.has_ipv6());
        assert!(summary.stun_status().starts_with("unverified"));
    }
}
