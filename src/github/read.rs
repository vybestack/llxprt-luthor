//! Bounded retries for idempotent gh API reads only. Mutating commands never use this path.
use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
    thread,
    time::Duration,
};

const TRIES: usize = 3;
const MAX_WAIT: Duration = Duration::from_secs(5);

pub(crate) fn api(executable: &Path, args: &[&str]) -> std::io::Result<Output> {
    read_with(
        || {
            Command::new(executable)
                .args(args)
                .arg("--include")
                .output()
        },
        thread::sleep,
    )
}

fn read_with(
    mut read: impl FnMut() -> std::io::Result<Output>,
    mut sleep: impl FnMut(Duration),
) -> std::io::Result<Output> {
    for attempt in 0..TRIES {
        let mut output = read()?;
        let (body, retry_after) = headers(&output.stdout);
        let status = http_status(&output.stdout, &output.stderr);
        output.stdout = body.to_vec();
        if let Some(status) = status {
            output
                .stderr
                .extend_from_slice(format!("\nHTTP {status}\n").as_bytes());
        }
        if output.status.success() || !transient(status, &output.stderr) || attempt + 1 == TRIES {
            return Ok(output);
        }
        let wait = retry_after.unwrap_or(Duration::from_millis(100 << attempt));
        // Do not violate a long Retry-After by retrying early or stall the entire cycle.
        if wait > MAX_WAIT {
            return Ok(output);
        }
        sleep(wait);
    }
    unreachable!("bounded read loop always returns its final result")
}

fn transient(status: Option<u16>, stderr: &[u8]) -> bool {
    matches!(status, Some(429 | 500..=599))
        || String::from_utf8_lossy(stderr)
            .to_ascii_lowercase()
            .contains("connection reset")
}

pub(crate) fn http_status(stdout: &[u8], stderr: &[u8]) -> Option<u16> {
    let (body, _) = headers(stdout);
    let json = serde_json::from_slice::<Value>(body).ok();
    json.as_ref()
        .and_then(|value| value.get("status"))
        .and_then(|value| {
            value
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| value.as_u64().and_then(|s| u16::try_from(s).ok()))
        })
        .or_else(|| {
            let text = format!(
                "{} {}",
                String::from_utf8_lossy(stdout),
                String::from_utf8_lossy(stderr)
            );
            let words = text.split_whitespace().collect::<Vec<_>>();
            words.windows(2).find_map(|pair| {
                (pair[0] == "HTTP" || pair[0].starts_with("HTTP/"))
                    .then(|| {
                        pair[1]
                            .trim_matches(|ch: char| !ch.is_ascii_digit())
                            .parse()
                            .ok()
                    })
                    .flatten()
            })
        })
}

fn headers(mut bytes: &[u8]) -> (&[u8], Option<Duration>) {
    let mut retry_after = None;
    while bytes.starts_with(b"HTTP/") {
        let end = bytes
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|n| (n, 4))
            .or_else(|| bytes.windows(2).position(|w| w == b"\n\n").map(|n| (n, 2)));
        let Some((end, separator)) = end else { break };
        for line in String::from_utf8_lossy(&bytes[..end]).lines() {
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("retry-after")
            {
                retry_after = retry_delay(value.trim());
            }
        }
        bytes = &bytes[end + separator..];
    }
    (bytes, retry_after)
}

fn retry_delay(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    // HTTP-date (IMF-fixdate), parsed without locale or timezone dependencies.
    let words = value.split_whitespace().collect::<Vec<_>>();
    if words.len() != 6 || words[5] != "GMT" {
        return None;
    }
    let day = words[1].parse::<i64>().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| *m == words[2])? as i64
        + 1;
    let year = words[3].parse::<i64>().ok()?;
    if !(1970..=9999).contains(&year) {
        return None;
    }
    let clock = words[4]
        .split(':')
        .map(str::parse::<i64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let [hour, minute, second] = clock.as_slice() else {
        return None;
    };
    if !(1..=31).contains(&day)
        || !(0..24).contains(hour)
        || !(0..60).contains(minute)
        || !(0..60).contains(second)
    {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let days =
        era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * m + 2) / 5 + day - 1 - 719468;
    let timestamp = days * 86400 + hour * 3600 + minute * 60 + second;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(Duration::from_secs(
        u64::try_from(timestamp).ok()?.saturating_sub(now),
    ))
}

#[cfg(test)]
mod tests;
