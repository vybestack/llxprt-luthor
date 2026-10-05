use luthor::state::launches;
pub(super) fn bounded_sanitized_output(path: &std::path::Path) -> String {
    use std::fs;

    let bytes = fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let mut sanitized = String::new();
    let mut redact_next = false;
    for word in text.split_whitespace() {
        if redact_next {
            sanitized.push_str("[REDACTED]");
            redact_next = false;
        } else if word.eq_ignore_ascii_case("authorization:") || word.eq_ignore_ascii_case("bearer")
        {
            sanitized.push_str(word);
            sanitized.push(' ');
            sanitized.push_str("[REDACTED]");
            redact_next = true;
        } else {
            sanitized.push_str(word);
        }
        sanitized.push(' ');
    }
    if bytes.len() > 4096 {
        sanitized.push_str("[truncated]");
    }
    sanitized
}

pub(super) fn wait_for_stop_receipt(receipt_path: &std::path::Path) {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !receipt_path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        receipt_path.exists(),
        "real rs stop produced no durable receipt"
    );
}

pub(super) fn report_stop_request_failure(
    dir: &tempfile::TempDir,
    config: &luthor::config::Config,
    store: &luthor::state::StateStore,
) {
    use std::fs;

    let receipt_path = config
        .state_root
        .join("attempts/installed-stop.receipt.json");
    let intent = launches::launch_intent(store, "installed-stop").unwrap();
    eprintln!(
        "STOP diagnostic: fixture={}, receipt_exists={}, launch_intent={}",
        dir.path().display(),
        receipt_path.exists(),
        intent.as_deref().map(|_| "present").unwrap_or("absent")
    );
    if let Ok(bytes) = fs::read(&receipt_path)
        && let Ok(receipt) = serde_json::from_slice::<luthor::supervisor::ExitReceipt>(&bytes)
    {
        eprintln!(
            "STOP receipt: exit_code={:?}, signal={:?}, stop_signals={:?}, stdout_bytes={}, stderr_bytes={}",
            receipt.exit_code,
            receipt.signal,
            receipt.stop_signals,
            receipt.stdout_bytes,
            receipt.stderr_bytes
        );
        for (label, path) in [
            ("stdout", &receipt.stdout_path),
            ("stderr", &receipt.stderr_path),
        ] {
            let text = fs::read_to_string(path).unwrap_or_default();
            let safe_lines = text
                .lines()
                .map(|line| {
                    if line.to_ascii_lowercase().contains("token")
                        || line.to_ascii_lowercase().contains("authorization")
                    {
                        "[redacted sensitive log line]"
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            eprintln!("STOP {label} log:\n{safe_lines}");
        }
    }
}

pub(super) fn stream_tail(path: &std::path::Path) -> String {
    use std::fs;

    let contents = fs::read_to_string(path).unwrap_or_default();
    let tail = contents.chars().rev().take(3000).collect::<String>();
    let tail = tail.chars().rev().collect::<String>();
    tail.lines()
        .map(|line| {
            if line.to_ascii_lowercase().contains("authorization")
                || line.to_ascii_lowercase().contains("api-key")
            {
                "[redacted authentication line]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
