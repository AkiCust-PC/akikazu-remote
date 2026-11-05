//! End-to-end test of two peers on loopback.
//!
//! This exercises the same path as the manual program: non-trickle SDP
//! exchange, ICE host candidates, DTLS, SCTP, DataChannel open and Ping/Pong.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use tokio::sync::mpsc::UnboundedReceiver;
use webrtc::data_channel::{DataChannel, DataChannelEvent};

use akikazu_remote::network::diagnostics::detect_selected_path;
use akikazu_remote::network::peer::{NetworkConfig, Peer, PeerState};
use akikazu_remote::protocol::WireMessage;

fn loopback_config() -> NetworkConfig {
    NetworkConfig {
        stun_servers: Vec::new(),
        bind_addrs: vec!["127.0.0.1:0".to_string()],
        gather_timeout: Duration::from_secs(4),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_peers_connect_and_exchange_ping_pong() -> Result<()> {
    let mut offerer = Peer::new(loopback_config()).await?;
    let mut answerer = Peer::new(loopback_config()).await?;

    let offer_channel = offerer.create_data_channel().await?;
    let offer = offerer.pc.create_offer(None).await?;
    offerer.pc.set_local_description(offer).await?;
    offerer.wait_for_gathering().await?;
    let offer_sdp = offerer
        .pc
        .local_description()
        .await
        .ok_or_else(|| anyhow!("offerer has no local description"))?;

    answerer.pc.set_remote_description(offer_sdp).await?;
    let answer = answerer.pc.create_answer(None).await?;
    answerer.pc.set_local_description(answer).await?;
    answerer.wait_for_gathering().await?;
    let answer_sdp = answerer
        .pc
        .local_description()
        .await
        .ok_or_else(|| anyhow!("answerer has no local description"))?;
    offerer.pc.set_remote_description(answer_sdp).await?;

    let answer_channel = answerer.wait_for_channel(Duration::from_secs(10)).await?;

    wait_open(&offer_channel, &mut offerer.state_rx).await?;
    wait_open(&answer_channel, &mut answerer.state_rx).await?;

    let ping = WireMessage::Ping {
        seq: 1,
        sent_unix_ms: 123,
    };
    offer_channel.send_text(&ping.encode()?).await?;
    let received = next_text(&answer_channel).await?;
    assert_eq!(WireMessage::decode(&received)?, ping);

    let pong = WireMessage::Pong {
        seq: 1,
        sent_unix_ms: 123,
    };
    answer_channel.send_text(&pong.encode()?).await?;
    let received = next_text(&offer_channel).await?;
    assert_eq!(WireMessage::decode(&received)?, pong);

    let path = wait_for_path(&offerer).await?;
    assert_eq!(path.family, "ipv4");
    assert!(path.packets_sent > 0);

    let _ = offer_channel.close().await;
    let _ = offerer.pc.close().await;
    let _ = answerer.pc.close().await;
    Ok(())
}

async fn wait_open(
    channel: &Arc<dyn DataChannel>,
    state_rx: &mut UnboundedReceiver<PeerState>,
) -> Result<()> {
    let wait = async {
        loop {
            tokio::select! {
                event = channel.poll() => match event {
                    Some(DataChannelEvent::OnOpen) => return Ok(()),
                    Some(DataChannelEvent::OnClose) | None => {
                        bail!("data channel closed before open")
                    }
                    _ => {}
                },
                Some(state) = state_rx.recv() => {
                    if matches!(state, PeerState::Failed | PeerState::Closed) {
                        bail!("peer failed before data channel open: {state}");
                    }
                }
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .map_err(|_| anyhow!("timed out waiting for data channel open"))?
}

async fn next_text(channel: &Arc<dyn DataChannel>) -> Result<String> {
    let wait = async {
        loop {
            match channel.poll().await {
                Some(DataChannelEvent::OnMessage(message)) if message.is_string => {
                    return Ok(String::from_utf8_lossy(&message.data).into_owned());
                }
                Some(DataChannelEvent::OnClose) | None => {
                    bail!("data channel closed while waiting for a message")
                }
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .map_err(|_| anyhow!("timed out waiting for a text message"))?
}

async fn wait_for_path(peer: &Peer) -> Result<akikazu_remote::network::diagnostics::SelectedPath> {
    for _ in 0..20 {
        if let Some(path) = detect_selected_path(&peer.pc).await {
            return Ok(path);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("no selected ICE candidate pair in stats")
}
