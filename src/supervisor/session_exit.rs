//! Session incompatibility is terminal for this session, not retryable failure.
use super::SupervisorError;
use crate::{
    model::{ExitReceipt, LaunchPlan, Reconciliation},
    state::{StateStore, journal, task_records},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
};

pub(crate) fn hold_incompatible(
    store: &mut StateStore,
    plan: &LaunchPlan,
    receipt: &ExitReceipt,
) -> Result<Option<Reconciliation>, SupervisorError> {
    if receipt.exit_code.is_none_or(|code| code == 0) || !receipt.stop_signals.is_empty() {
        return Ok(None);
    }
    let saved = journal::evidence_payload(
        store,
        &plan.task_id,
        Some(&plan.attempt_id),
        "session_incompatible",
    )?;
    let reported = if saved.is_some() {
        Some(None)
    } else {
        diagnostic(&receipt.stdout_path)?.or(diagnostic(&receipt.stderr_path)?)
    };
    let Some(version) = reported else {
        return Ok(None);
    };
    let mut first: LaunchPlan =
        serde_json::from_str(&crate::state::initial_launch(store, &plan.task_id)?)?;
    first.worker_build = crate::state::pin_adoption::effective_build(&store.connection, &first)?;
    let label = |plan: &LaunchPlan| {
        plan.worker_build.as_ref().map_or_else(
            || format!("{} (hash unrecorded)", plan.executable.display()),
            |build| build.label(),
        )
    };
    let reason = format!(
        "session incompatible; pinned worker {}; attempted worker {}; restore a compatible build or explicitly migrate the session; retries blocked",
        label(&first),
        label(plan)
    );
    if saved.is_none() {
        journal::record_evidence(
            store,
            &plan.task_id,
            Some(&plan.attempt_id),
            "session_incompatible",
            &json!({
                "class":"session_incompatible", "pinned_worker":first.worker_build,
                "attempted_worker":plan.worker_build, "reported_state_version":version
            })
            .to_string(),
        )?;
    }
    if task_records::task_phase(store, &plan.task_id)?.as_deref() != Some("held")
        || task_records::held_reason(store, &plan.task_id)?.as_deref() != Some(&reason)
    {
        task_records::hold_task(store, &plan.task_id, &reason)?;
    }
    Ok(Some(Reconciliation::Held { reason }))
}

pub(crate) fn record_format(
    store: &mut StateStore,
    plan: &LaunchPlan,
    receipt: &ExitReceipt,
) -> Result<(), SupervisorError> {
    if journal::evidence_payload(
        store,
        &plan.task_id,
        Some(&plan.attempt_id),
        "session_state_format",
    )?
    .is_some()
    {
        return Ok(());
    }
    for path in [&receipt.stdout_path, &receipt.stderr_path] {
        for line in BufReader::new(open_log(path)?).split(b'\n') {
            let line = line?;
            let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if let Some(version) = value.get("session_state_version").and_then(Value::as_u64) {
                journal::record_evidence(
                    store,
                    &plan.task_id,
                    Some(&plan.attempt_id),
                    "session_state_format",
                    &json!({
                        "version":version, "worker_build":plan.worker_build
                    })
                    .to_string(),
                )?;
                return Ok(());
            }
        }
    }
    Ok(())
}

fn open_log(path: &Path) -> Result<fs::File, SupervisorError> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}

fn diagnostic(path: &Path) -> Result<Option<Option<String>>, SupervisorError> {
    let file = open_log(path)?;
    for line in BufReader::new(file).split(b'\n') {
        let line = line?;
        if let Some(version) = incompatible_line(&line) {
            return Ok(Some(version));
        }
    }
    Ok(None)
}

fn incompatible_line(line: &[u8]) -> Option<Option<String>> {
    if let Ok(value) = serde_json::from_slice::<Value>(line) {
        let error = value.get("error")?.as_object()?;
        let code = error.get("code").and_then(Value::as_str);
        if matches!(code, Some("session_incompatible" | "session-incompatible")) {
            return Some(None);
        }
        return unsupported_version(error.get("message")?.as_str()?);
    }
    let text = std::str::from_utf8(line).ok()?.trim();
    if text.starts_with("corrupt session state:") || text.starts_with("error:") {
        return unsupported_version(text);
    }
    None
}

fn unsupported_version(message: &str) -> Option<Option<String>> {
    let (_, version) = message.split_once("unsupported state version ")?;
    let version: String = version.chars().take_while(char::is_ascii_digit).collect();
    (!version.is_empty()).then_some(Some(version))
}

#[cfg(test)]
mod tests {
    use super::incompatible_line;
    #[test]
    fn structured_and_plain_session_diagnostics_not_transient_or_prompt_mentions() {
        assert_eq!(incompatible_line(br#"{"error":{"code":"turn","message":"corrupt session state: unsupported state version 3"}}"#), Some(Some("3".into())));
        assert_eq!(
            incompatible_line(
                br#"{"error":{"code":"session_incompatible","message":"cannot load session"}}"#
            ),
            Some(None)
        );
        assert_eq!(
            incompatible_line(b"error: corrupt session state: unsupported state version 12"),
            Some(Some("12".into()))
        );
        assert_eq!(
            incompatible_line(b"Please fix unsupported state version 3"),
            None
        );
        assert_eq!(
            incompatible_line(br#"{"error":{"code":"turn","message":"rate limited"}}"#),
            None
        );
    }
}
