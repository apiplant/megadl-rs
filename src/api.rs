//! MEGA API client: JSON command calls with EAGAIN retry and
//! X-Hashcash proof-of-work.

use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::crypto::{b64decode, b64encode};
use crate::http;

pub const DEFAULT_API_URL: &str = "https://g.api.mega.co.nz/cs";
const SRV_EAGAIN: i64 = -3;

fn srv_error_name(code: i64) -> &'static str {
    match code {
        -1 => "EINTERNAL",
        -2 => "EARGS",
        -3 => "EAGAIN",
        -4 => "ERATELIMIT",
        -5 => "EFAILED",
        -6 => "ETOOMANY",
        -7 => "ERANGE",
        -8 => "EEXPIRED",
        -9 => "ENOENT",
        -10 => "ECIRCULAR",
        -11 => "EACCESS",
        -12 => "EEXIST",
        -13 => "EINCOMPLETE",
        -14 => "EKEY",
        -15 => "ESID",
        -16 => "EBLOCKED",
        -17 => "EOVERQUOTA",
        -18 => "ETEMPUNAVAIL",
        -19 => "ETOOMANYCONNECTIONS",
        _ => "EUNKNOWN",
    }
}

fn srv_error(code: i64) -> String {
    let name = srv_error_name(code);
    if name == "EUNKNOWN" {
        format!("server returned error EUNKNOWN ({code})")
    } else {
        format!("server returned error {name}")
    }
}

pub struct ApiClient {
    pub url: String,
    pub folder: String,
    seq: AtomicU32,
}

impl ApiClient {
    pub fn new(url: &str, folder: &str) -> Self {
        ApiClient { url: url.to_string(), folder: folder.to_string(), seq: AtomicU32::new(0) }
    }

    /// Posts one command and decodes the first element of the reply,
    /// retrying EAGAIN (and dropped connections / 500s) with
    /// exponential backoff.
    pub fn call(&self, cmd: serde_json::Value) -> Result<serde_json::Value, String> {
        let body = serde_json::to_vec(&[cmd]).map_err(|e| e.to_string())?;
        let mut delay = Duration::from_millis(250);
        loop {
            match self.post(&body) {
                Ok(data) => match decode_api_response(&data)? {
                    ApiOutcome::Value(v) => return Ok(v),
                    ApiOutcome::Retry => {}
                },
                Err(RetryableErr::Retry(_msg)) => {}
                Err(RetryableErr::Fatal(msg)) => return Err(msg),
            }
            if delay > Duration::from_secs(256) {
                return Err("server keeps asking us to retry, giving up".into());
            }
            thread::sleep(delay);
            delay *= 2;
        }
    }

    fn post(&self, body: &[u8]) -> Result<Vec<u8>, RetryableErr> {
        let mut url = format!("{}?id={}", self.url, self.seq.fetch_add(1, Ordering::Relaxed));
        if !self.folder.is_empty() {
            url.push_str("&n=");
            url.push_str(&self.folder);
        }
        let mut hashcash: Option<String> = None;
        for _ in 0..4 {
            let mut headers: Vec<(&str, &str)> = Vec::new();
            if let Some(h) = &hashcash {
                headers.push(("X-Hashcash", h));
            }
            let resp = http::post(&url, &headers, body, false)
                .map_err(|e| RetryableErr::Retry(e.to_string()))?;
            match resp.status {
                402 => {
                    // Proof-of-work gate: solve the challenge and replay the
                    // request with the answer in X-Hashcash.
                    let challenge = resp
                        .header("X-Hashcash")
                        .ok_or_else(|| RetryableErr::Fatal("402 without an X-Hashcash challenge".into()))?
                        .to_string();
                    hashcash = Some(
                        solve_hashcash(&challenge)
                            .map_err(|e| RetryableErr::Fatal(format!("hashcash challenge: {e}")))?,
                    );
                    continue;
                }
                500 => return Err(RetryableErr::Retry("server returned 500 (probably busy)".into())),
                200 | 201 => return Ok(resp.body.into_buffered()),
                other => return Err(RetryableErr::Fatal(format!("server returned {other}"))),
            }
        }
        Err(RetryableErr::Fatal("too many hashcash retries".into()))
    }
}

enum RetryableErr {
    Retry(String),
    Fatal(String),
}

enum ApiOutcome {
    Value(serde_json::Value),
    Retry,
}

/// Handles the API's reply shapes: a bare negative number
/// (request-level error), or an array whose first element is the
/// result object or a negative number.
fn decode_api_response(data: &[u8]) -> Result<ApiOutcome, String> {
    let trim = std::str::from_utf8(data).unwrap_or("").trim();
    if let Ok(code) = trim.parse::<i64>() {
        if code == SRV_EAGAIN {
            return Ok(ApiOutcome::Retry);
        }
        if code < 0 {
            return Err(srv_error(code));
        }
        return Err(format!("unexpected API response {trim}"));
    }
    let elems: Vec<serde_json::Value> =
        serde_json::from_str(trim).map_err(|_| "invalid API response".to_string())?;
    let first = elems.first().ok_or_else(|| "invalid API response".to_string())?;
    if let Some(code) = first.as_i64() {
        if code < 0 {
            return Err(srv_error(code));
        }
        return Err(format!("unexpected API response {trim}"));
    }
    Ok(ApiOutcome::Value(first.clone()))
}

/// Solves MEGA's X-Hashcash v1 proof-of-work challenge
/// ("1:<easiness>:<junk>:<token>"): finds a 4-byte prefix such that the
/// leading 32 bits of sha256(prefix || token x 262144) are under the
/// easiness threshold. Response header is "1:<token>:<b64 prefix>".
pub fn solve_hashcash(challenge: &str) -> Result<String, String> {
    let parts: Vec<&str> = challenge.split(':').collect();
    if parts.len() != 4 || parts[0] != "1" {
        return Err(format!("unsupported challenge {challenge:?}"));
    }
    let easiness: i32 = parts[1].parse().map_err(|_| "bad easiness")?;
    if !(0..=255).contains(&easiness) {
        return Err("bad easiness".into());
    }
    let token = b64decode(parts[3])?;
    if token.len() != 48 {
        return Err("bad token".into());
    }
    let easiness = easiness as u32;
    let threshold: u32 = ((easiness & 63) << 1 | 1) << ((easiness >> 6) * 7 + 3);

    let mut buf = vec![0u8; 4 + 262144 * 48];
    for i in 0..262144 {
        buf[4 + i * 48..4 + i * 48 + 48].copy_from_slice(&token);
    }
    for prefix in 1u32..=u32::MAX {
        buf[0..4].copy_from_slice(&prefix.to_le_bytes());
        let sum = Sha256::digest(&buf);
        let head = u32::from_be_bytes([sum[0], sum[1], sum[2], sum[3]]);
        if head <= threshold {
            return Ok(format!("1:{}:{}", parts[3], b64encode(&prefix.to_le_bytes())));
        }
    }
    Err("hashcash search exhausted".into())
}
