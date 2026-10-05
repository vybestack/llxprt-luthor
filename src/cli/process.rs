use super::CliError;
#[cfg(unix)]
use super::files::{attempts_dir, private_file};
use crate::cli_identifiers::valid_attempt;
#[cfg(unix)]
use crate::supervisor::{ChildIdentity, LaunchPlan, recorded_process, verified_live_process};
use rusqlite::Connection;
use serde_json::Value;
#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use std::fs;
use std::path::Path;

#[cfg(unix)]
fn live_process(conn: &Connection, root: &Path, task: &str, attempt: &str) -> Option<Value> {
    use std::os::unix::fs::PermissionsExt;
    let dir = attempts_dir(root).ok()?;
    if fs::metadata(&dir).ok()?.permissions().mode() & 0o777 != 0o700 {
        return None;
    }
    let plan = launch_plan(&dir, task, attempt)?;
    if !authorized_launch(conn, root, task, attempt, &plan)?
        || dir.join(format!("{attempt}.receipt.json")).exists()
        || dir
            .join(format!("{attempt}.supervisor-error.json"))
            .exists()
    {
        return None;
    }
    let (child_file, _) = private_file(&dir.join(format!("{attempt}.child.json"))).ok()?;
    let child: ChildIdentity = serde_json::from_reader(child_file).ok()?;
    if serde_json::from_str::<ChildIdentity>(&evidence(
        conn,
        task,
        attempt,
        "evidence",
        "child_registered",
    )?)
    .ok()?
        != child
    {
        return None;
    }
    let supervisor = recorded_process(&evidence(
        conn,
        task,
        attempt,
        "evidence",
        "supervisor_ready",
    )?)?;
    if recorded_process(&evidence(conn, task, attempt, "evidence", "gate_sent")?)? != supervisor
        || recorded_process(&evidence(conn, task, attempt, "intents", "gate_release")?)?
            != supervisor
        || !verified_live_process(&child, &supervisor)
    {
        return None;
    }
    Some(json!({"attempt_id":attempt,
        "child":{"pid":child.pid,"group_id":child.group_id,
        "boot_identity":child.boot_identity,"start_identity":child.start_identity},
        "supervisor":{"pid":supervisor.pid,"group_id":supervisor.pid,
        "boot_identity":supervisor.boot_identity,"start_identity":supervisor.start_identity}}))
}

pub(crate) fn operator_state(
    conn: &Connection,
    root: &Path,
    task: &str,
    attempt: Option<&str>,
    phase: &str,
) -> Result<(String, Option<Value>), CliError> {
    if phase != "held" {
        return Ok((phase.to_owned(), None));
    }
    let Some(attempt) = attempt.filter(|id| valid_attempt(id)) else {
        return Ok((phase.to_owned(), None));
    };
    let stopped: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='stop')",
        [task, attempt], |r| r.get(0)
    ).map_err(|_| CliError::Database)?;
    if stopped {
        return Ok(("stop_requested".into(), None));
    }
    #[cfg(unix)]
    if let Some(process) = live_process(conn, root, task, attempt) {
        // Recheck after probing the OS so a concurrent stop cannot be reported as running.
        let stopped: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='stop')",
            [task, attempt], |r| r.get(0)
        ).map_err(|_| CliError::Database)?;
        if stopped {
            return Ok(("stop_requested".into(), None));
        }
        return Ok(("running".into(), Some(process)));
    }
    #[cfg(not(unix))]
    let _ = root;
    Ok(("held".into(), None))
}

#[cfg(unix)]
fn authorized_launch(
    conn: &Connection,
    root: &Path,
    task: &str,
    attempt: &str,
    plan: &LaunchPlan,
) -> Option<bool> {
    let valid: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts a JOIN reservations r ON r.attempt_id=a.id AND r.task_id=a.task_id
         JOIN tasks t ON t.id=a.task_id WHERE a.id=?1 AND a.task_id=?2 AND a.lifecycle='launch_intended'
         AND a.outcome IS NULL AND r.status='reserved' AND t.state='held'
         AND (SELECT COUNT(*) FROM intents WHERE task_id=?2 AND attempt_id=?1 AND kind='launch')=1
         AND (SELECT COUNT(*) FROM intents WHERE task_id=?2 AND attempt_id=?1 AND kind='supervisor_dispatch')=1
         AND (SELECT COUNT(*) FROM intents WHERE task_id=?2 AND attempt_id=?1 AND kind='gate_release')=1
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND attempt_id=?1 AND kind='child_registered')=1
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND attempt_id=?1 AND kind='supervisor_ready')=1
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND attempt_id=?1 AND kind='gate_sent')=1
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND attempt_id=?1 AND kind='log_failure')=0)",
        rusqlite::params![attempt, task], |r| r.get(0)
    ).ok()?;
    if !valid {
        return Some(false);
    }
    observed_plan_binding(conn, root, plan)
}

#[cfg(unix)]
fn observed_plan_binding(conn: &Connection, root: &Path, plan: &LaunchPlan) -> Option<bool> {
    use crate::state::{amended_dispatch::has_amendment, verify_amended_observation_plan};
    if has_amendment(conn, &plan.task_id, &plan.attempt_id).ok()? {
        // This binds an existing dispatch for display only, never resume/retry eligibility.
        return Some(verify_amended_observation_plan(conn, root, plan).is_ok());
    }
    let expected = serde_json::to_string(plan).ok()?;
    Some(
        evidence(conn, &plan.task_id, &plan.attempt_id, "intents", "launch")? == expected
            && evidence(
                conn,
                &plan.task_id,
                &plan.attempt_id,
                "intents",
                "supervisor_dispatch",
            )? == expected,
    )
}

#[cfg(unix)]
fn evidence(
    conn: &Connection,
    task: &str,
    attempt: &str,
    table: &str,
    kind: &str,
) -> Option<String> {
    let sql = match table {
        "evidence" => "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind=?3",
        "intents" => "SELECT detail FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind=?3",
        _ => unreachable!(),
    };
    conn.query_row(sql, [task, attempt, kind], |r| r.get(0))
        .ok()
}

#[cfg(unix)]
fn launch_plan(dir: &Path, task: &str, attempt: &str) -> Option<LaunchPlan> {
    let (plan_file, _) = private_file(&dir.join(format!("{attempt}.plan.json"))).ok()?;
    let plan: LaunchPlan = serde_json::from_reader(plan_file).ok()?;
    if plan.task_id != task || plan.attempt_id != attempt || plan.session_id != task {
        return None;
    }
    Some(plan)
}
