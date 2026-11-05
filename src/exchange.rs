//! Manual signaling token handling.
//!
//! Phase 0 deliberately avoids a signaling server. The complete SDP, including
//! ICE candidates and the DTLS fingerprint, is serialized, compressed only by
//! base64, and copied by the operator. The token is a secret while the session
//! is being established: anyone who obtains it before the connection is
//! authenticated could attempt to answer. This is acceptable for a lab test and
//! is exactly what the signaling phase will replace.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::Serialize;
use serde::de::DeserializeOwned;
use webrtc::peer_connection::RTCSessionDescription;

pub const TOKEN_PREFIX: &str = "AKR1.";

pub fn encode_payload<T: Serialize>(value: &T) -> Result<String> {
    let json = serde_json::to_vec(value).context("serialize signaling payload")?;
    Ok(format!(
        "{TOKEN_PREFIX}{}",
        STANDARD.encode(json.as_slice())
    ))
}

pub fn decode_payload<T: DeserializeOwned>(token: &str) -> Result<T> {
    let compact: String = token.chars().filter(|c| !c.is_whitespace()).collect();
    let compact = compact
        .strip_prefix(TOKEN_PREFIX)
        .unwrap_or(compact.as_str());
    if compact.is_empty() {
        bail!("empty signaling token");
    }

    let bytes = STANDARD
        .decode(compact)
        .or_else(|_| URL_SAFE_NO_PAD.decode(compact))
        .context("signaling token is not valid base64")?;
    serde_json::from_slice(&bytes).context("signaling token is not valid JSON")
}

pub fn encode_description(description: &RTCSessionDescription) -> Result<String> {
    encode_payload(description)
}

pub fn decode_description(token: &str) -> Result<RTCSessionDescription> {
    decode_payload(token)
}

pub fn write_token(path: &Path, token: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create directory {}", parent.display()))?;
        }
    }

    let temporary = temporary_path(path);
    fs::write(&temporary, format!("{}\n", token.trim()))
        .with_context(|| format!("write {}", temporary.display()))?;
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("replace {}", path.display()))?;
    }
    fs::rename(&temporary, path)
        .with_context(|| format!("move {} to {}", temporary.display(), path.display()))
}

pub fn read_token(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("read signaling token from {}", path.display()))?;
    if text.trim().is_empty() {
        bail!("signaling token file {} is empty", path.display());
    }
    Ok(text)
}

pub async fn wait_for_token_file(path: &Path, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        match read_token(path) {
            Ok(token) => return Ok(token),
            Err(error) if Instant::now() >= deadline => {
                return Err(error).with_context(|| {
                    format!(
                        "timed out after {}s waiting for signaling token {}",
                        timeout.as_secs(),
                        path.display()
                    )
                });
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub async fn prompt_for_token(prompt: &str) -> Result<String> {
    eprintln!("{prompt}");
    let line = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("read signaling token from stdin")?;
        Ok::<_, anyhow::Error>(line)
    })
    .await
    .context("stdin reader task failed")??;

    if line.trim().is_empty() {
        bail!("no signaling token was entered");
    }
    Ok(line)
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(".tmp");
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_roundtrip_and_whitespace_tolerance() {
        let value = json!({"type": "offer", "sdp": "v=0\r\n"});
        let token = encode_payload(&value).expect("encode");
        assert!(token.starts_with(TOKEN_PREFIX));
        let wrapped = format!("\n  {}  \n", token);
        let decoded: serde_json::Value = decode_payload(&wrapped).expect("decode");
        assert_eq!(decoded, value);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode_payload::<serde_json::Value>("not-a-token").is_err());
        assert!(decode_payload::<serde_json::Value>("").is_err());
    }

    #[test]
    fn token_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("akikazu-remote-test-{}", std::process::id()));
        let path = dir.join("offer.txt");
        write_token(&path, "AKR1.example").expect("write");
        assert_eq!(read_token(&path).expect("read").trim(), "AKR1.example");
        let _ = fs::remove_dir_all(&dir);
    }
}
