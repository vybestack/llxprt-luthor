//! Launch planning and the Unix gated child runner.
use crate::{
    config::{Config, RenderedCommand, TaskValues},
    state::{ExitPrEvidence, RetryAuthorization, SelectionEvidence, StateStore, WorktreeIdentity},
    worker_instructions::prompt,
    worktree::{self},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{
    env,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
#[cfg(unix)]
use std::{
    os::unix::{
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    process::Child,
};

mod binding;
#[cfg(unix)]
mod binding_gate;
mod command;
mod launch;
#[cfg(unix)]
mod worker;
pub use launch::execute;
#[cfg(unix)]
pub use launch::{execute_amended, execute_amended_with_binary, execute_with_binary};
#[cfg(unix)]
pub use worker::worker_gate;
#[cfg(unix)]
use worker::{configure_session, verify_launch_worktree};
mod storage;
use command::{PromptRequirements, enforce_prompt, requires_pair};
#[cfg(unix)]
pub use storage::validate_stop_socket_path;
use storage::{private_file, write_private_json};
#[cfg(unix)]
use storage::{stop_socket, write_private_json_atomic};
mod error;
mod evidence;
mod processes;
#[cfg(unix)]
mod readiness;
mod reconciliation;
mod recovery;
mod terminal_exit;
#[cfg(unix)]
pub(crate) use crate::model::{ChildIdentity, ProcessIdentity, recorded_process};
pub use crate::model::{
    ExitReceipt, LaunchPlan, Reconciliation, RecoveryInspection, SessionEnvironment,
    TerminalExitProof,
};
pub use command::validate_saved_initial_plan;
pub use error::SupervisorError;
#[cfg(unix)]
pub(crate) use processes::verified_live_process;
use processes::*;
pub(crate) use reconciliation::recheck_retry_exit;
pub use reconciliation::reconcile_attempt;
pub use recovery::inspect_recovery_quiescence;
pub use storage::{inspect_never_dispatched_storage, preflight_attempt_storage};

impl SessionEnvironment {
    pub fn capture() -> Result<Self, SupervisorError> {
        let names = [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "LLXPRT_CONFIG_HOME",
        ];
        let values: std::collections::BTreeMap<_, _> = names
            .into_iter()
            .filter_map(|name| env::var_os(name).map(|value| (name.to_owned(), value)))
            .collect();
        let home = values
            .get("HOME")
            .map(PathBuf::from)
            .ok_or(SupervisorError::Conflict)?;
        Self::capture_from(home, &values)
    }

    fn capture_from(
        home: PathBuf,
        values: &std::collections::BTreeMap<String, std::ffi::OsString>,
    ) -> Result<Self, SupervisorError> {
        if !home.is_absolute() {
            return Err(SupervisorError::Conflict);
        }
        let home = fs::canonicalize(home)?;
        fn absolute_override(
            name: &str,
            values: &std::collections::BTreeMap<String, std::ffi::OsString>,
        ) -> Result<Option<PathBuf>, SupervisorError> {
            match values.get(name).map(PathBuf::from) {
                None => Ok(None),
                Some(path) if path.is_absolute() => Ok(Some(path)),
                Some(_) => Err(SupervisorError::Conflict),
            }
        }
        Ok(Self {
            home,
            xdg_config_home: absolute_override("XDG_CONFIG_HOME", values)?,
            xdg_data_home: absolute_override("XDG_DATA_HOME", values)?,
            xdg_state_home: absolute_override("XDG_STATE_HOME", values)?,
            llxprt_config_home: absolute_override("LLXPRT_CONFIG_HOME", values)?,
        })
    }

    fn matches(&self, current: &Self) -> bool {
        self == current
    }

    pub fn matches_current(&self) -> Result<bool, SupervisorError> {
        Ok(self.matches(&Self::capture()?))
    }
}

#[cfg(test)]
mod session_environment_tests {
    use super::{SessionEnvironment, SupervisorError};
    use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

    fn values(entries: &[(&str, &str)]) -> BTreeMap<String, OsString> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
            .collect()
    }

    fn capture(
        home: &str,
        entries: &[(&str, &str)],
    ) -> Result<SessionEnvironment, SupervisorError> {
        SessionEnvironment::capture_from(PathBuf::from(home), &values(entries))
    }

    fn temp_home(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("llxprt-session-environment-{name}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn capture_canonicalizes_home_and_compares_all_overrides() {
        let home = temp_home("same");
        let home_text = home.to_str().unwrap();
        let env = capture(home_text, &[("HOME", home_text)]).unwrap();
        let same = capture(home_text, &[("HOME", home_text)]).unwrap();
        assert_eq!(env, same);
        assert!(env.matches(&same));

        let other_home = temp_home("other");
        assert!(
            !env.matches(
                &capture(
                    other_home.to_str().unwrap(),
                    &[("HOME", other_home.to_str().unwrap())]
                )
                .unwrap()
            )
        );

        let xdg_a = capture(home_text, &[("XDG_CONFIG_HOME", "/tmp/config-a")]).unwrap();
        let xdg_b = capture(home_text, &[("XDG_CONFIG_HOME", "/tmp/config-b")]).unwrap();
        assert!(!xdg_a.matches(&xdg_b));

        let llxprt_a = capture(home_text, &[("LLXPRT_CONFIG_HOME", "/tmp/llxprt-a")]).unwrap();
        let llxprt_b = capture(home_text, &[("LLXPRT_CONFIG_HOME", "/tmp/llxprt-b")]).unwrap();
        assert!(!llxprt_a.matches(&llxprt_b));
    }

    #[test]
    fn capture_rejects_missing_home_and_relative_paths() {
        assert!(matches!(
            SessionEnvironment::capture_from(PathBuf::from("relative-home"), &BTreeMap::new()),
            Err(SupervisorError::Conflict)
        ));
        assert!(matches!(
            capture("relative-home", &[]),
            Err(SupervisorError::Conflict)
        ));

        let home = temp_home("relative-overrides");
        let home_text = home.to_str().unwrap();
        assert!(matches!(
            capture(home_text, &[("XDG_CONFIG_HOME", "relative-config")]),
            Err(SupervisorError::Conflict)
        ));
    }
}

fn enforce_worktree_inspection(args: &mut [String], previous: &str) -> Result<(), SupervisorError> {
    let indexes: Vec<usize> = args
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| matches!(pair[0].as_str(), "-p" | "--prompt").then_some(index))
        .collect();
    if indexes.len() != 1 {
        return Err(SupervisorError::Conflict);
    }
    args[indexes[0] + 1].push_str(&format!(
        "\n\nBefore continuing, inspect the files left in the worktree by the {previous}. Do not assume its transcript was restored; use the files as the source of truth for what remains to be done.",
    ));
    Ok(())
}

pub fn ensure_distinct_resume_prompt(
    first: &LaunchPlan,
    latest: &LaunchPlan,
    args: &[String],
) -> Result<(), SupervisorError> {
    let continuation = prompt(args).ok_or(SupervisorError::Conflict)?;
    if prompt(&first.args) == Some(continuation) || prompt(&latest.args) == Some(continuation) {
        return Err(SupervisorError::Conflict);
    }
    Ok(())
}

/// Renders one initial attempt; the caller remains responsible for fresh claim
/// and absent-PR evidence. This function deliberately cannot start a worker.
pub fn prepare_initial(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<LaunchPlan, SupervisorError> {
    validate_stop_socket_path(store.root(), attempt_id)?;
    let selection = store.claimed_worktree_context(task_id)?;
    let identity = worktree::verify_existing_worktree(store, task_id)?;
    let RenderedCommand { executable, args } =
        command::initial_command(&selection, &identity, task_id, attempt_id)?;
    let worktree = identity.path.clone();
    let session_environment = SessionEnvironment::capture()?;
    let plan = LaunchPlan {
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        session_id: task_id.to_owned(),
        worktree,
        expected_worktree: identity,
        executable,
        args,
        config_revision: selection.config_revision,
        session_environment,
    };
    store.hold_launch_intent(task_id, attempt_id, &serde_json::to_string(&plan)?)?;
    Ok(plan)
}

/// Renders a continuation for a verified paused task without starting a worker.
/// The new attempt must preserve the original session and verified worktree.
pub fn prepare_resume(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<LaunchPlan, SupervisorError> {
    let (first, latest, _) = store.resume_context(task_id)?;
    let first: LaunchPlan = serde_json::from_str(&first)?;
    let latest: LaunchPlan = serde_json::from_str(&latest)?;
    if !first.session_environment.matches_current()? {
        return Err(SupervisorError::Conflict);
    }
    let selection = store
        .selection_evidence(task_id)?
        .ok_or(SupervisorError::Conflict)?;
    let identity = worktree::verify_existing_worktree(store, task_id)?;
    if first.task_id != task_id
        || latest.task_id != task_id
        || first.session_id != task_id
        || latest.session_id != task_id
        || first.worktree != identity.path
        || latest.worktree != identity.path
        || !worktree::matches_snapshot(&first.expected_worktree, &identity)?
        || !worktree::matches_snapshot(&latest.expected_worktree, &identity)?
        || first.config_revision != selection.config_revision
        || latest.config_revision != selection.config_revision
    {
        return Err(SupervisorError::Conflict);
    }
    let worktree = identity.path.clone();
    let cwd = worktree.to_str().ok_or(SupervisorError::Conflict)?;
    let values = TaskValues {
        task_issue_number: selection.candidate.issue_number.to_string(),
        task_repository: selection.candidate.repository.clone(),
        task_issue_url: selection.candidate.issue_url.clone(),
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        worktree: cwd.to_owned(),
    };
    let RenderedCommand {
        executable,
        mut args,
    } = selection.effective_config.resume.render(&values)?;
    let raw_continuation = prompt(&args).ok_or(SupervisorError::Conflict)?;
    if selection.effective_config.resume.args == selection.effective_config.initial.args
        || !requires_pair(&args, "--session", task_id)
        || !requires_pair(&args, "--cwd", cwd)
        || raw_continuation.trim().is_empty()
    {
        return Err(SupervisorError::Conflict);
    }
    enforce_prompt(
        &mut args,
        PromptRequirements {
            issue: &selection.candidate,
            code_repository: &selection.candidate.mapping.code_repository,
            base: &identity.base,
            head_repository: &selection.candidate.mapping.allowed_pr_head_repository,
            branch: &identity.branch,
            remote: &identity.remote,
            author: &selection.candidate.mapping.allowed_pr_author,
            assignee: &selection.effective_config.assignment_login,
        },
    )?;
    enforce_worktree_inspection(&mut args, "interrupted or canceled turn")?;
    ensure_distinct_resume_prompt(&first, &latest, &args)?;
    let plan = LaunchPlan {
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        session_id: task_id.to_owned(),
        worktree,
        expected_worktree: identity,
        executable,
        args,
        config_revision: selection.config_revision,
        session_environment: first.session_environment.clone(),
    };
    store.hold_launch_intent(task_id, attempt_id, &serde_json::to_string(&plan)?)?;
    Ok(plan)
}

pub(crate) struct RetryPlanAuthorization<'a> {
    pub attempt_id: &'a str,
    pub config: &'a Config,
    pub revision: &'a str,
    pub actor: &'a str,
    pub reason: &'a str,
    pub pr: ExitPrEvidence,
    pub source: crate::state::RetrySourceEvidence,
    pub terminal_exit: Option<TerminalExitProof>,
}

/// Renders a separately authorized natural-exit continuation without starting a worker.
pub(crate) fn prepare_retry(
    store: &mut StateStore,
    previous: &LaunchPlan,
    authorization: RetryPlanAuthorization<'_>,
) -> Result<LaunchPlan, SupervisorError> {
    let RetryPlanAuthorization {
        attempt_id,
        config,
        revision,
        actor,
        reason,
        pr,
        source,
        terminal_exit,
    } = authorization;
    let task_id = previous.task_id.as_str();
    let first = crate::state::initial_launch(store, task_id)?;
    let first: LaunchPlan = serde_json::from_str(&first)?;
    let latest = previous;
    if !first.session_environment.matches_current()? {
        return Err(SupervisorError::Conflict);
    }
    let mut selection = crate::state::selection_for_attempt(store, previous)?;
    let previous_config = selection.effective_config.clone();
    selection.effective_config = config.into();
    let identity = worktree::verify_existing_worktree(store, task_id)?;
    if first.task_id != task_id
        || latest.task_id != task_id
        || first.session_id != task_id
        || latest.session_id != task_id
        || first.worktree != identity.path
        || latest.worktree != identity.path
        || !worktree::matches_snapshot(&first.expected_worktree, &identity)?
        || !worktree::matches_snapshot(&latest.expected_worktree, &identity)?
    {
        return Err(SupervisorError::Conflict);
    }
    let RenderedCommand { executable, args } =
        retry_command(&selection, task_id, attempt_id, &identity, &first, latest)?;
    let worktree = identity.path.clone();
    let plan = LaunchPlan {
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        session_id: task_id.to_owned(),
        worktree,
        expected_worktree: identity,
        executable,
        args,
        config_revision: revision.to_owned(),
        session_environment: first.session_environment.clone(),
    };
    crate::state::hold_retry_intent(
        store,
        &RetryAuthorization {
            actor: actor.to_owned(),
            reason: reason.to_owned(),
            previous_plan: previous.clone(),
            previous_config,
            config: config.into(),
            plan: plan.clone(),
            reservation: attempt_id.to_owned(),
            pr,
            source,
            terminal_exit,
        },
    )?;
    Ok(plan)
}

#[cfg(unix)]
fn finish_receipt(
    root: &Path,
    plan: &LaunchPlan,
    receipt: &ExitReceipt,
    managed: bool,
) -> Result<(), SupervisorError> {
    if managed {
        let state_root = root.parent().ok_or(SupervisorError::Conflict)?;
        let db = Connection::open_with_flags(
            state_root.join("state.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        binding::verify_observed_plan(&db, state_root, plan)?;
    }
    write_receipt(root, &plan.attempt_id, receipt)
}

fn write_receipt(root: &Path, attempt: &str, receipt: &ExitReceipt) -> Result<(), SupervisorError> {
    let final_path = root.join(format!("{attempt}.receipt.json"));
    let temp_path = root.join(format!(".{attempt}.receipt.tmp"));
    let mut file = private_file(&temp_path)?;
    serde_json::to_writer(&mut file, receipt)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temp_path, &final_path)?;
    File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn peer_pid(stream: &UnixStream) -> Result<u32, SupervisorError> {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "macos")]
    {
        let mut pid: libc::pid_t = 0;
        let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&raw mut pid).cast(),
                &raw mut len,
            )
        } != 0
            || len as usize != std::mem::size_of_val(&pid)
            || pid <= 0
        {
            return Err(SupervisorError::StopUnavailable);
        }
        Ok(pid as u32)
    }
    #[cfg(target_os = "linux")]
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut cred).cast(),
                &raw mut len,
            )
        } != 0
            || len as usize != std::mem::size_of_val(&cred)
            || cred.pid <= 0
        {
            return Err(SupervisorError::StopUnavailable);
        }
        Ok(cred.pid as u32)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err(SupervisorError::StopUnavailable)
}

/// The coordinator may signal only a registered, currently verified dedicated
/// child group after the recorded supervisor has ceased matching its identity.
#[cfg(unix)]
fn stop_without_supervisor(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    supervisor: &ProcessIdentity,
) -> Result<(), SupervisorError> {
    let release = store
        .intent_payload(task_id, attempt_id, "gate_release")?
        .and_then(|payload| recorded_process(&payload))
        .ok_or(SupervisorError::StopUnavailable)?;
    if &release != supervisor
        || binding::verify_attempt_artifact(&store.connection, store.root(), task_id, attempt_id)
            .is_err()
    {
        return Err(SupervisorError::StopUnavailable);
    }
    let registered = store
        .evidence_payload(task_id, Some(attempt_id), "child_registered")?
        .and_then(|payload| serde_json::from_str::<ChildIdentity>(&payload).ok())
        .ok_or(SupervisorError::StopUnavailable)?;
    let attempts = store.root().join("attempts");
    use std::os::unix::fs::PermissionsExt;
    let dir = fs::symlink_metadata(&attempts).map_err(|_| SupervisorError::StopUnavailable)?;
    if !dir.file_type().is_dir() || dir.permissions().mode() & 0o777 != 0o700 {
        return Err(SupervisorError::StopUnavailable);
    }
    let child = private_bytes(&attempts.join(format!("{attempt_id}.child.json")))
        .and_then(|bytes| serde_json::from_slice::<ChildIdentity>(&bytes).ok())
        .ok_or(SupervisorError::StopUnavailable)?;
    if child != registered || child.pid == supervisor.pid || !matching_child(&child) {
        return Err(SupervisorError::StopUnavailable);
    }
    let pid = i32::try_from(child.pid).map_err(|_| SupervisorError::StopUnavailable)?;
    let target = serde_json::json!({"pid":child.pid,"group_id":child.group_id,
        "boot_identity":child.boot_identity,"start_identity":child.start_identity});
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGKILL] {
        // Revalidate the recorded boot, start, and dedicated process group before
        // every escalation. A PID alone is never a safe signal target.
        if !matching_child(&child) {
            return Err(SupervisorError::StopUnavailable);
        }
        let decision = serde_json::json!({"target":target,"signal":signal});
        store.record_evidence(
            task_id,
            Some(attempt_id),
            "independent_stop_decision",
            &decision.to_string(),
        )?;
        if !matching_child(&child) {
            return Err(SupervisorError::StopUnavailable);
        }
        let sent = unsafe { libc::kill(-pid, signal) } == 0;
        let result = serde_json::json!({"target":target,"signal":signal,"sent":sent});
        store.record_evidence(
            task_id,
            Some(attempt_id),
            "independent_stop_signal",
            &result.to_string(),
        )?;
        if !sent {
            return Err(SupervisorError::StopUnavailable);
        }
        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            if group_absent(pid) {
                let absence = serde_json::json!({"target":target,"probe":"kill(-pgid, 0): ESRCH"});
                store.record_evidence(
                    task_id,
                    Some(attempt_id),
                    "independent_group_absent",
                    &absence.to_string(),
                )?;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    Err(SupervisorError::StopUnavailable)
}

/// A durable stop request awaiting the process-identity and signal decision.
#[cfg(unix)]
#[must_use]
pub struct PendingStop<'a> {
    store: &'a mut StateStore,
    task_id: &'a str,
    attempt_id: &'a str,
}

/// Commit the intent separately so exit between intent and signaling is testable.
#[cfg(unix)]
pub fn prepare_stop<'a>(
    store: &'a mut StateStore,
    task_id: &'a str,
    attempt_id: &'a str,
) -> Result<PendingStop<'a>, SupervisorError> {
    if !valid_attempt(attempt_id) {
        return Err(SupervisorError::Conflict);
    }
    store.record_stop_intent(task_id, attempt_id)?;
    Ok(PendingStop {
        store,
        task_id,
        attempt_id,
    })
}

/// Persist the request before contacting the supervisor.
#[cfg(unix)]
pub fn request_stop(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<(), SupervisorError> {
    prepare_stop(store, task_id, attempt_id)?.finish()
}

#[cfg(unix)]
impl PendingStop<'_> {
    pub fn finish(self) -> Result<(), SupervisorError> {
        let Self {
            store,
            task_id,
            attempt_id,
        } = self;
        let recorded = store
            .evidence_payload(task_id, Some(attempt_id), "supervisor_ready")?
            .as_deref()
            .and_then(recorded_process)
            .ok_or(SupervisorError::StopUnavailable)?;
        match identity(recorded.pid) {
            Ok((boot, start))
                if boot == recorded.boot_identity && start == recorded.start_identity =>
            {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                if zombie(recorded.pid) {
                    return stop_without_supervisor(store, task_id, attempt_id, &recorded);
                }
            }
            Ok(_) => return stop_without_supervisor(store, task_id, attempt_id, &recorded),
            Err(_) => {
                let pid =
                    i32::try_from(recorded.pid).map_err(|_| SupervisorError::StopUnavailable)?;
                // A failed identity lookup alone is not proof of death.
                if unsafe { libc::kill(pid, 0) } == 0 {
                    #[cfg(any(target_os = "macos", target_os = "linux"))]
                    if zombie(recorded.pid) {
                        return stop_without_supervisor(store, task_id, attempt_id, &recorded);
                    }
                    return Err(SupervisorError::StopUnavailable);
                }
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                    return Err(SupervisorError::StopUnavailable);
                }
                return stop_without_supervisor(store, task_id, attempt_id, &recorded);
            }
        }
        let path = stop_socket(&store.root().join("attempts"), attempt_id);
        let mut stream = UnixStream::connect(path).map_err(|_| SupervisorError::StopUnavailable)?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        if peer_pid(&stream)? != recorded.pid {
            return Err(SupervisorError::StopUnavailable);
        }
        stream.write_all(format!("{task_id}\n{attempt_id}\n").as_bytes())?;
        let mut answer = [0];
        if stream.read_exact(&mut answer).is_err() || answer[0] != b'Y' {
            return Err(SupervisorError::StopUnavailable);
        }
        Ok(())
    }
}

#[cfg(not(unix))]
pub fn request_stop(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<(), SupervisorError> {
    store.record_stop_intent(task_id, attempt_id)?;
    Err(SupervisorError::StopUnavailable)
}

#[cfg(unix)]
fn stop_child_matches(pid: i32, boot: &str, start: &str) -> bool {
    identity(pid as u32).ok().as_ref() == Some(&(boot.to_owned(), start.to_owned()))
        && unsafe { libc::getpgid(pid) } == pid
}

#[cfg(unix)]
fn handle_stop(
    stream: &mut UnixStream,
    plan: &LaunchPlan,
    store_root: &Path,
    child: &mut Child,
    boot: &str,
    start: &str,
    signals: &mut Vec<i32>,
) -> Result<(), SupervisorError> {
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let mut request = [0u8; 512];
    let size = stream.read(&mut request).unwrap_or(0);
    let expected = format!("{}\n{}\n", plan.task_id, plan.attempt_id);
    let connection = Connection::open_with_flags(
        store_root
            .parent()
            .ok_or(SupervisorError::Conflict)?
            .join("state.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let persisted: Vec<String> = connection
        .prepare("SELECT detail FROM intents WHERE kind='stop' AND task_id=?1 AND attempt_id=?2")?
        .query_map(params![plan.task_id, plan.attempt_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let intended =
        serde_json::json!({"task_id":plan.task_id,"attempt_id":plan.attempt_id}).to_string();
    let pid = i32::try_from(child.id()).map_err(|_| SupervisorError::StopUnavailable)?;
    if binding::verify_observed_plan(&connection, &store_root.join(".."), plan).is_err()
        || request[..size] != *expected.as_bytes()
        || persisted != [intended]
        || child.try_wait()?.is_some()
        || !stop_child_matches(pid, boot, start)
    {
        stream.write_all(b"N")?;
        return Ok(());
    }
    // Each escalation targets only the still-matching dedicated process group.
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGKILL] {
        if child.try_wait()?.is_some() {
            stream.write_all(b"Y")?;
            return Ok(());
        }
        if !stop_child_matches(pid, boot, start) {
            stream.write_all(b"N")?;
            return Ok(());
        }
        if unsafe { libc::kill(-pid, signal) } != 0 {
            stream.write_all(b"N")?;
            return Ok(());
        }
        signals.push(signal);
        let deadline = Instant::now() + Duration::from_millis(400);
        while Instant::now() < deadline {
            if child.try_wait()?.is_some() {
                stream.write_all(b"Y")?;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    if child.try_wait()?.is_some() {
        stream.write_all(b"Y")?;
    } else {
        stream.write_all(b"N")?;
    }
    Ok(())
}

/// Waits for one explicit `R` byte before starting the worker. EOF keeps the
/// already-reserved attempt held and launches nothing. Unix process groups are
/// used so later reconciliation can independently prove group termination.
#[cfg(unix)]
pub fn run_gated_child<R: Read>(
    plan: &LaunchPlan,
    gate: R,
    store_root: &Path,
) -> Result<ExitStatus, SupervisorError> {
    run_gated_child_with_binary(plan, gate, store_root, &env::current_exe()?)
}

#[cfg(unix)]
pub fn run_gated_child_with_binary<R: Read>(
    plan: &LaunchPlan,
    gate: R,
    store_root: &Path,
    binary: &Path,
) -> Result<ExitStatus, SupervisorError> {
    run_gated_child_control(plan, gate, store_root, None, binary, |out, err| (out, err))
}

/// Allows a controlled log writer to be injected without changing filesystem-wide behavior.
#[cfg(unix)]
pub fn run_gated_child_with_log_writers<R, O, E>(
    plan: &LaunchPlan,
    gate: R,
    store_root: &Path,
    binary: &Path,
    writers: impl FnOnce(File, File) -> (O, E),
) -> Result<ExitStatus, SupervisorError>
where
    R: Read,
    O: Write + Send + 'static,
    E: Write + Send + 'static,
{
    run_gated_child_control(plan, gate, store_root, None, binary, writers)
}

#[cfg(unix)]
fn record_log_failure(
    plan: &LaunchPlan,
    store_root: &Path,
    stream: &str,
    error: &str,
) -> Result<(), SupervisorError> {
    let path = store_root
        .parent()
        .ok_or(SupervisorError::Conflict)?
        .join("state.sqlite3");
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let tx = connection.transaction()?;
    let detail =
        serde_json::json!({"task_id":plan.task_id,"attempt_id":plan.attempt_id}).to_string();
    let persisted: Option<String> = tx
        .query_row(
            "SELECT detail FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='stop'",
            params![plan.task_id, plan.attempt_id],
            |row| row.get(0),
        )
        .optional()?;
    if persisted.as_ref().is_some_and(|prior| prior != &detail) {
        return Err(SupervisorError::Conflict);
    }
    if persisted.is_none() {
        tx.execute(
            "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'stop',?4)",
            params![
                format!("stop-{}", plan.attempt_id),
                plan.task_id,
                plan.attempt_id,
                detail
            ],
        )?;
    }
    let payload = serde_json::json!({"stream":stream,"error":error}).to_string();
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'log_failure',?3)",
        params![plan.task_id, plan.attempt_id, payload],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(unix)]
fn stop_failed_log_child(
    child: &mut Child,
    registered: &ChildIdentity,
) -> Result<(), SupervisorError> {
    let pid = i32::try_from(registered.pid).map_err(|_| SupervisorError::StopUnavailable)?;
    stop_failed_log_child_with(
        Duration::from_secs(2),
        || matching_child(registered),
        || unsafe { libc::kill(-pid, libc::SIGKILL) } == 0,
        || Ok(child.try_wait()?.is_some()),
        || group_absent(pid),
    )
}

#[cfg(unix)]
fn stop_failed_log_child_with(
    limit: Duration,
    mut matching: impl FnMut() -> bool,
    mut kill_group: impl FnMut() -> bool,
    mut try_wait: impl FnMut() -> Result<bool, SupervisorError>,
    mut absent: impl FnMut() -> bool,
) -> Result<(), SupervisorError> {
    let deadline = Instant::now() + limit;
    loop {
        // Send SIGKILL immediately while the recorded leader still proves group
        // ownership. After it exits, its identity cannot authorize another kill.
        let owned = matching();
        if owned {
            let _ = kill_group();
        }
        let exited = try_wait()?;
        if absent() && exited {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(SupervisorError::StopUnavailable);
        }
        thread::sleep(
            Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(all(test, unix))]
mod failed_log_stop_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn tracked_current_process_prevents_absence_proof() {
        let (boot_identity, start_identity) = identity(std::process::id()).unwrap();
        let child = ChildIdentity {
            pid: u32::MAX - 10,
            boot_identity: boot_identity.clone(),
            start_identity: "synthetic-child-start".to_owned(),
            group_id: u32::MAX - 10,
        };
        let supervisor = ProcessIdentity {
            pid: std::process::id(),
            boot_identity: boot_identity.clone(),
            start_identity,
        };
        let tracked = [ProcessIdentity {
            pid: std::process::id(),
            boot_identity,
            start_identity: "tracked-current-process".to_owned(),
        }];
        assert!(!registered_processes_absent(&child, &supervisor, &tracked));
    }

    #[test]
    fn invalid_tracked_boot_is_rejected_before_absence_checks() {
        let (boot_identity, start_identity) = identity(std::process::id()).unwrap();
        let child = ChildIdentity {
            pid: u32::MAX - 10,
            boot_identity: boot_identity.clone(),
            start_identity: "synthetic-child-start".to_owned(),
            group_id: u32::MAX - 10,
        };
        let supervisor = ProcessIdentity {
            pid: std::process::id(),
            boot_identity,
            start_identity,
        };
        let tracked = [ProcessIdentity {
            pid: u32::MAX - 11,
            boot_identity: "different-boot".to_owned(),
            start_identity: "tracked-start".to_owned(),
        }];
        assert!(!registered_processes_absent(&child, &supervisor, &tracked));
    }

    #[test]
    fn malformed_tracked_process_is_rejected() {
        assert!(recorded_process("not-json").is_none());
    }

    #[test]
    fn leader_exit_before_signal_keeps_group_held_without_signaling_reused_identity() {
        let signals = Cell::new(0);
        let result = stop_failed_log_child_with(
            Duration::ZERO,
            || false,
            || {
                signals.set(signals.get() + 1);
                true
            },
            || Ok(true),
            || false,
        );
        assert!(matches!(result, Err(SupervisorError::StopUnavailable)));
        assert_eq!(signals.get(), 0);
    }

    #[test]
    fn failed_first_signal_retries_only_with_proven_leader_and_requires_group_absence() {
        let signals = Cell::new(0);
        let result = stop_failed_log_child_with(
            Duration::from_millis(100),
            || true,
            || {
                signals.set(signals.get() + 1);
                signals.get() == 2
            },
            || Ok(signals.get() >= 2),
            || signals.get() >= 2,
        );
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(signals.get(), 2);
    }

    #[test]
    fn transient_identity_failure_does_not_abandon_signalable_child() {
        let probes = Cell::new(0);
        let signals = Cell::new(0);
        let result = stop_failed_log_child_with(
            Duration::from_millis(100),
            || {
                probes.set(probes.get() + 1);
                probes.get() > 1
            },
            || {
                signals.set(signals.get() + 1);
                true
            },
            || Ok(signals.get() == 1),
            || signals.get() == 1,
        );
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(signals.get(), 1);
    }

    #[test]
    fn reaped_leader_with_remaining_group_is_never_reported_stopped() {
        let signals = Cell::new(0);
        let result = stop_failed_log_child_with(
            Duration::from_millis(100),
            || signals.get() == 0,
            || {
                signals.set(signals.get() + 1);
                true
            },
            || Ok(true),
            || false,
        );
        assert!(matches!(result, Err(SupervisorError::StopUnavailable)));
        assert_eq!(signals.get(), 1);
    }
}

#[cfg(unix)]
fn drain_worker_log(
    reader: impl Read,
    mut writer: impl Write,
    sync: &File,
) -> std::io::Result<u64> {
    let bytes = std::io::copy(&mut std::io::BufReader::new(reader), &mut writer)?;
    writer.flush()?;
    sync.sync_all()?;
    Ok(bytes)
}

#[cfg(unix)]
fn run_gated_child_control<R, O, E>(
    plan: &LaunchPlan,
    mut gate: R,
    store_root: &Path,
    control: Option<&UnixListener>,
    binary: &Path,
    writers: impl FnOnce(File, File) -> (O, E),
) -> Result<ExitStatus, SupervisorError>
where
    R: Read,
    O: Write + Send + 'static,
    E: Write + Send + 'static,
{
    if !valid_attempt(&plan.attempt_id) {
        return Err(SupervisorError::Conflict);
    }
    fs::create_dir_all(store_root)?;
    let stdout_path = store_root.join(format!("{}.stdout.log", plan.attempt_id));
    let stderr_path = store_root.join(format!("{}.stderr.log", plan.attempt_id));
    let stdout_file = private_file(&stdout_path)?;
    let stderr_file = private_file(&stderr_path)?;
    let out_sync = stdout_file.try_clone()?;
    let err_sync = stderr_file.try_clone()?;
    let plan_path = store_root.join(format!("{}.plan.json", plan.attempt_id));
    if control.is_none() {
        write_private_json(&plan_path, plan)?;
    }
    let mut command = Command::new(binary);
    command
        .arg("__worker_gate")
        // The worker gate changes cwd to the task worktree; preserve the plan's identity.
        .arg(fs::canonicalize(&plan_path)?)
        .current_dir(&plan.worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_session(&mut command, &plan.session_environment);
    command.process_group(0);
    let mut child = command.spawn()?;
    let mut shim_gate = child.stdin.take().expect("piped shim gate");
    let registered = (|| -> Result<ChildIdentity, SupervisorError> {
        let (boot_identity, start_identity) = identity(child.id())?;
        let pid = i32::try_from(child.id()).map_err(|_| SupervisorError::IdentityUnavailable)?;
        if unsafe { libc::getpgid(pid) } != pid {
            return Err(SupervisorError::IdentityUnavailable);
        }
        let identity = ChildIdentity {
            pid: child.id(),
            boot_identity,
            start_identity,
            group_id: child.id(),
        };
        write_private_json_atomic(
            &store_root.join(format!("{}.child.json", plan.attempt_id)),
            &identity,
        )?;
        if control.is_some() {
            println!("READY");
            let mut poll = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 5000) } <= 0 {
                return Err(SupervisorError::GateClosed);
            }
        }
        let mut release = [0];
        if gate.read(&mut release)? != 1 || release != *b"R" {
            return Err(SupervisorError::GateClosed);
        }
        if control.is_some() {
            worker::verify_gate_release(store_root, plan, &identity)?;
        }
        shim_gate.write_all(b"R")?;
        Ok(identity)
    })();
    let registered = match registered {
        Ok(identity) => identity,
        Err(error) => {
            drop(shim_gate);
            child.wait()?;
            return Err(error);
        }
    };
    let boot_identity = registered.boot_identity.clone();
    let child_start_identity = registered.start_identity.clone();
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (mut out_file, mut err_file) = writers(stdout_file, stderr_file);
    let (sender, receiver) = mpsc::channel();
    let out_sender = sender.clone();
    let out_thread = thread::spawn(move || {
        let result = drain_worker_log(stdout, &mut out_file, &out_sync);
        let _ = out_sender.send(("stdout", result));
    });
    let err_thread = thread::spawn(move || {
        let result = drain_worker_log(stderr, &mut err_file, &err_sync);
        let _ = sender.send(("stderr", result));
    });
    let mut stop_signals = Vec::new();
    let mut status = None;
    let mut stdout_bytes = None;
    let mut stderr_bytes = None;
    let mut exited_at = None;
    while status.is_none() || stdout_bytes.is_none() || stderr_bytes.is_none() {
        if status.is_none() {
            status = child.try_wait()?;
            if status.is_some() {
                exited_at = Some(Instant::now());
            }
        }
        while let Ok((stream, result)) = receiver.try_recv() {
            let bytes = match result {
                Ok(bytes) => bytes,
                Err(error) => {
                    let stopped = stop_failed_log_child(&mut child, &registered);
                    let evidence = record_log_failure(plan, store_root, stream, &error.to_string());
                    stopped?;
                    evidence?;
                    return Err(SupervisorError::ExecutionUnavailable);
                }
            };
            match stream {
                "stdout" => stdout_bytes = Some(bytes),
                "stderr" => stderr_bytes = Some(bytes),
                _ => unreachable!(),
            }
        }
        if status.is_some() && stdout_bytes.is_some() && stderr_bytes.is_some() {
            break;
        }
        if exited_at.is_some_and(|at| at.elapsed() > Duration::from_secs(2)) {
            return Err(SupervisorError::ExecutionUnavailable);
        }
        if status.is_none()
            && let Some(listener) = control
        {
            match listener.accept() {
                Ok((mut stream, _)) => handle_stop(
                    &mut stream,
                    plan,
                    store_root,
                    &mut child,
                    &boot_identity,
                    &child_start_identity,
                    &mut stop_signals,
                )?,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    out_thread
        .join()
        .map_err(|_| SupervisorError::ExecutionUnavailable)?;
    err_thread
        .join()
        .map_err(|_| SupervisorError::ExecutionUnavailable)?;
    let status = status.expect("worker exited");
    let stdout_bytes = stdout_bytes.expect("stdout drained");
    let stderr_bytes = stderr_bytes.expect("stderr drained");
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    let receipt = ExitReceipt {
        attempt_id: plan.attempt_id.clone(),
        child_pid: registered.pid,
        boot_identity,
        child_start_identity,
        exit_code: status.code(),
        signal,
        stdout_path,
        stdout_bytes,
        stderr_path,
        stderr_bytes,
        stop_signals,
    };
    finish_receipt(store_root, plan, &receipt, control.is_some())?;
    Ok(status)
}

/// Non-Unix execution is intentionally unavailable until process-group semantics exist.
#[cfg(not(unix))]
pub fn run_gated_child<R: Read>(
    _plan: &LaunchPlan,
    _gate: R,
    _store_root: &Path,
) -> Result<ExitStatus, SupervisorError> {
    Err(SupervisorError::ExecutionUnavailable)
}

/// The child opens SQLite read-only, without taking the coordinator's process lock.
/// It verifies the exact launch plan, reservation and verified claim/worktree before READY.
pub fn supervise(root: &Path, attempt: &str) -> Result<(), SupervisorError> {
    if !valid_attempt(attempt) {
        return Err(SupervisorError::Conflict);
    }
    let attempts = root.join("attempts");
    let result = (|| {
        let plan: LaunchPlan =
            serde_json::from_slice(&fs::read(attempts.join(format!("{attempt}.plan.json")))?)?;
        if plan.attempt_id != attempt
            || plan.session_id != plan.task_id
            || plan.config_revision.is_empty()
        {
            return Err(SupervisorError::Conflict);
        }
        let connection = Connection::open_with_flags(
            root.join("state.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let row: Option<(String, String)> = connection.query_row(
            "SELECT i.detail, (SELECT payload FROM evidence WHERE task_id=?2 AND kind='worktree_created')
             FROM intents i JOIN attempts a ON a.id=i.attempt_id
             JOIN reservations r ON r.attempt_id=a.id JOIN tasks t ON t.id=a.task_id
             WHERE i.kind='launch' AND i.attempt_id=?1 AND i.task_id=?2
               AND r.task_id=?2 AND r.status='reserved' AND t.state='held'
               AND (SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND kind='supervisor_dispatch')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND kind='claim_verified')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND kind='worktree_created')=1",
            params![attempt, plan.task_id], |row| Ok((row.get(0)?, row.get(1)?))
        ).optional()?;
        let (_, worktree) = row.ok_or(SupervisorError::Conflict)?;
        binding::verify_worker_plan(&connection, root, &plan)?;
        let identity: WorktreeIdentity = serde_json::from_str(&worktree)?;
        verify_launch_worktree(&connection, &plan)?;
        if !worktree::matches_snapshot(&identity, &plan.expected_worktree)?
            || identity.path != plan.worktree
            || fs::canonicalize(&plan.worktree)? != plan.worktree
        {
            return Err(SupervisorError::Conflict);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::metadata(&plan.worktree)?;
            if metadata.dev() != identity.device || metadata.ino() != identity.inode {
                return Err(SupervisorError::Conflict);
            }
        }
        #[cfg(unix)]
        let listener = {
            let socket = stop_socket(&attempts, attempt);
            let listener = UnixListener::bind(socket)?;
            listener.set_nonblocking(true)?;
            listener
        };
        #[cfg(unix)]
        run_gated_child_control(
            &plan,
            std::io::stdin(),
            &attempts,
            Some(&listener),
            &env::current_exe()?,
            |out, err| (out, err),
        )?;
        #[cfg(not(unix))]
        run_gated_child(&plan, std::io::stdin(), &attempts)?;
        Ok(())
    })();
    if let Err(error) = &result {
        // The coordinator retains the reservation whether the gate closed or the worker failed.
        let _ = write_private_json(
            &attempts.join(format!("{attempt}.supervisor-error.json")),
            &serde_json::json!({"attempt_id":attempt,"error":error.to_string()}),
        );
    }
    result
}

fn retry_command(
    selection: &SelectionEvidence,
    task_id: &str,
    attempt_id: &str,
    identity: &WorktreeIdentity,
    first: &LaunchPlan,
    latest: &LaunchPlan,
) -> Result<RenderedCommand, SupervisorError> {
    let worktree = identity.path.clone();
    let cwd = worktree.to_str().ok_or(SupervisorError::Conflict)?;
    let values = TaskValues {
        task_issue_number: selection.candidate.issue_number.to_string(),
        task_repository: selection.candidate.repository.clone(),
        task_issue_url: selection.candidate.issue_url.clone(),
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        worktree: cwd.to_owned(),
    };
    let RenderedCommand {
        executable,
        mut args,
    } = selection.effective_config.resume.render(&values)?;
    let raw_continuation = prompt(&args).ok_or(SupervisorError::Conflict)?;
    if selection.effective_config.resume.args == selection.effective_config.initial.args
        || !requires_pair(&args, "--session", task_id)
        || !requires_pair(&args, "--cwd", cwd)
        || raw_continuation.trim().is_empty()
    {
        return Err(SupervisorError::Conflict);
    }
    enforce_prompt(
        &mut args,
        PromptRequirements {
            issue: &selection.candidate,
            code_repository: &selection.candidate.mapping.code_repository,
            base: &identity.base,
            head_repository: &selection.candidate.mapping.allowed_pr_head_repository,
            branch: &identity.branch,
            remote: &identity.remote,
            author: &selection.candidate.mapping.allowed_pr_author,
            assignee: &selection.effective_config.assignment_login,
        },
    )?;
    enforce_worktree_inspection(&mut args, "naturally exited worker")?;
    ensure_distinct_resume_prompt(first, latest, &args)?;
    Ok(RenderedCommand { executable, args })
}
