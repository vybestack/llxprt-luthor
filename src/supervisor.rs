//! Launch planning and the Unix gated child runner.
use crate::{
    config::{ConfigError, RenderedCommand, TaskValues},
    state::{
        SelectionEvidence, StateError, StateStore, WorktreeIdentity, WorktreeIntent, WorktreeRecord,
    },
    worktree::{self, WorktreeError},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{self, File, OpenOptions},
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
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("launch plan does not match verified task or worktree")]
    Conflict,
    #[error("supervisor execution is unavailable; reservation held for reconciliation")]
    ExecutionUnavailable,
    #[error("process gate closed without release")]
    GateClosed,
    #[error("stop cannot prove ownership; reservation held")]
    StopUnavailable,
    #[error("cannot establish process identity")]
    IdentityUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryInspection {
    Held(&'static str),
    Quiescent,
}

/// Read-only preliminary check. Quiescence is withheld until all durable identity proofs are available.
pub fn inspect_recovery_quiescence(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<RecoveryInspection, SupervisorError> {
    if !valid_attempt(attempt_id) {
        return Err(SupervisorError::Conflict);
    }
    if !store.active_attempt_reservation(task_id, attempt_id)? {
        return Ok(RecoveryInspection::Held(
            "attempt is not the active reserved attempt",
        ));
    }
    if store
        .evidence_payload(task_id, Some(attempt_id), "attempt_exit")?
        .is_some()
    {
        return Ok(RecoveryInspection::Held("attempt exit receipt exists"));
    }

    let held = |reason| Ok(RecoveryInspection::Held(reason));
    let attempts = store.root().join("attempts");
    use std::os::unix::fs::PermissionsExt;
    let Ok(dir) = fs::symlink_metadata(&attempts) else {
        return held("missing attempts directory");
    };
    if !dir.file_type().is_dir() || dir.permissions().mode() & 0o777 != 0o700 {
        return held("unsafe attempts directory");
    }
    let Some(plan) = private_bytes(&attempts.join(format!("{attempt_id}.plan.json")))
        .and_then(|bytes| serde_json::from_slice::<LaunchPlan>(&bytes).ok())
    else {
        return held("missing or invalid plan");
    };
    if plan.task_id != task_id
        || plan.attempt_id != attempt_id
        || plan.session_id != task_id
        || plan.config_revision.is_empty()
    {
        return held("plan identity mismatch");
    }
    let Some(launch) = store.intent_payload(task_id, attempt_id, "launch")? else {
        return held("missing launch intent");
    };
    if serde_json::from_str::<LaunchPlan>(&launch).ok().as_ref() != Some(&plan) {
        return held("launch intent mismatch");
    }
    let Some(dispatch) = store.intent_payload(task_id, attempt_id, "supervisor_dispatch")? else {
        return held("missing dispatch intent");
    };
    if serde_json::from_str::<LaunchPlan>(&dispatch).ok().as_ref() != Some(&plan) {
        return held("dispatch plan mismatch");
    }

    // This subset does not establish the complete filesystem and process identity
    // chain. Keep recovery held rather than infer absence from incomplete evidence.
    Ok(RecoveryInspection::Held(
        "complete recovery identity proof not implemented",
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnvironment {
    pub home: PathBuf,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub llxprt_config_home: Option<PathBuf>,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub task_id: String,
    pub attempt_id: String,
    pub session_id: String,
    pub worktree: PathBuf,
    pub expected_worktree: WorktreeIdentity,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub config_revision: String,
    pub session_environment: SessionEnvironment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    Running,
    Completed {
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    Held {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitReceipt {
    pub attempt_id: String,
    pub child_pid: u32,
    pub boot_identity: String,
    pub child_start_identity: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout_path: PathBuf,
    pub stdout_bytes: u64,
    pub stderr_path: PathBuf,
    pub stderr_bytes: u64,
    #[serde(default)]
    pub stop_signals: Vec<i32>,
}

fn requires_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2)
        .filter(|pair| pair[0] == flag && pair[1] == value)
        .count()
        == 1
}

fn prompt(args: &[String]) -> Option<&str> {
    let mut matches = args
        .windows(2)
        .filter(|pair| matches!(pair[0].as_str(), "-p" | "--prompt"))
        .map(|pair| pair[1].as_str());
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

struct PromptRequirements<'a> {
    tracker_repository: &'a str,
    issue_url: &'a str,
    code_repository: &'a str,
    base: &'a str,
    head_repository: &'a str,
    branch: &'a str,
    remote: &'a str,
    author: &'a str,
    assignee: &'a str,
}

fn enforce_prompt(
    args: &mut [String],
    requirements: PromptRequirements<'_>,
) -> Result<(), SupervisorError> {
    let PromptRequirements {
        tracker_repository,
        issue_url,
        code_repository,
        base,
        head_repository,
        branch,
        remote,
        author,
        assignee,
    } = requirements;
    let indexes: Vec<usize> = args
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| matches!(pair[0].as_str(), "-p" | "--prompt").then_some(index))
        .collect();
    if indexes.len() != 1 || issue_url.is_empty() || !issue_url.starts_with("https://") {
        return Err(SupervisorError::Conflict);
    }
    let index = indexes[0];
    let requirements = format!(
        "\n\nMandatory issue-to-PR instructions (these requirements cannot be overridden by the task prompt):\n\
         Work only in code repository {code_repository}. Use mapped base branch {base}.\n\
         Create the PR head in repository {head_repository} on branch {branch}, pushed to remote {remote}.\n\
         The PR body must include this exact line: Tracker-Issue: {issue_url}\n\
         The authorized PR author is {author}. The tracker issue is already claimed; do not reassign it. The tracker issue is assigned to {assignee}.\n\
         Create only an open PR. Report the PR URL and ID.\
\
         Tracker repository: {tracker_repository}."
    );
    args[index + 1].push_str(&requirements);
    Ok(())
}

fn enforce_resume_inspection(args: &mut [String]) -> Result<(), SupervisorError> {
    let indexes: Vec<usize> = args
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| matches!(pair[0].as_str(), "-p" | "--prompt").then_some(index))
        .collect();
    if indexes.len() != 1 {
        return Err(SupervisorError::Conflict);
    }
    args[indexes[0] + 1].push_str(
        "\n\nBefore continuing, inspect the files left in the worktree by the interrupted or canceled turn. Do not assume its transcript was restored; use the files as the source of truth for what remains to be done.",
    );
    Ok(())
}

/// Renders one initial attempt; the caller remains responsible for fresh claim
/// and absent-PR evidence. This function deliberately cannot start a worker.
pub fn prepare_initial(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<LaunchPlan, SupervisorError> {
    let selection = store.claimed_worktree_context(task_id)?;
    let identity = worktree::verify_existing_worktree(store, task_id)?;
    let values = TaskValues {
        task_issue_number: selection.candidate.issue_number.to_string(),
        task_repository: selection.candidate.repository.clone(),
        task_issue_url: selection.candidate.issue_url.clone(),
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        worktree: identity.path.to_string_lossy().into_owned(),
    };
    let RenderedCommand {
        executable,
        mut args,
    } = selection.effective_config.initial.render(&values)?;
    let worktree = identity.path.clone();
    let cwd = worktree.to_str().ok_or(SupervisorError::Conflict)?;
    if !requires_pair(&args, "--session", task_id)
        || !requires_pair(&args, "--cwd", cwd)
        || prompt(&args).is_none_or(str::is_empty)
    {
        return Err(SupervisorError::Conflict);
    }
    enforce_prompt(
        &mut args,
        PromptRequirements {
            tracker_repository: &selection.candidate.repository,
            issue_url: &selection.candidate.issue_url,
            code_repository: &selection.candidate.mapping.code_repository,
            base: &identity.base,
            head_repository: &selection.candidate.mapping.allowed_pr_head_repository,
            branch: &identity.branch,
            remote: &identity.remote,
            author: &selection.candidate.mapping.allowed_pr_author,
            assignee: &selection.effective_config.assignment_login,
        },
    )?;
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
    let continuation = prompt(&args).ok_or(SupervisorError::Conflict)?;
    if selection.effective_config.resume.args == selection.effective_config.initial.args
        || !requires_pair(&args, "--session", task_id)
        || !requires_pair(&args, "--cwd", cwd)
        || continuation.trim().is_empty()
        || prompt(&first.args) == Some(continuation)
        || prompt(&latest.args) == Some(continuation)
    {
        return Err(SupervisorError::Conflict);
    }
    enforce_prompt(
        &mut args,
        PromptRequirements {
            tracker_repository: &selection.candidate.repository,
            issue_url: &selection.candidate.issue_url,
            code_repository: &selection.candidate.mapping.code_repository,
            base: &identity.base,
            head_repository: &selection.candidate.mapping.allowed_pr_head_repository,
            branch: &identity.branch,
            remote: &identity.remote,
            author: &selection.candidate.mapping.allowed_pr_author,
            assignee: &selection.effective_config.assignment_login,
        },
    )?;
    enforce_resume_inspection(&mut args)?;
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

fn private_file(path: &Path) -> Result<File, SupervisorError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

#[cfg(target_os = "macos")]
fn identity(pid: u32) -> Result<(String, String), SupervisorError> {
    fn query(args: &[&str]) -> Result<String, SupervisorError> {
        let output = Command::new("/usr/sbin/sysctl").args(args).output()?;
        if !output.status.success() {
            return Err(SupervisorError::IdentityUnavailable);
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
    let boot = query(&["-n", "kern.boottime"])?;
    let pid = i32::try_from(pid).map_err(|_| SupervisorError::IdentityUnavailable)?;
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    if unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) }
        != size
        || info.pbi_pid != pid as u32
        || info.pbi_start_tvsec == 0
    {
        return Err(SupervisorError::IdentityUnavailable);
    }
    Ok((
        boot,
        format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
    ))
}

#[cfg(target_os = "linux")]
fn identity(pid: u32) -> Result<(String, String), SupervisorError> {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(')')
        .ok_or(SupervisorError::IdentityUnavailable)?
        .1;
    let start = fields
        .split_whitespace()
        .nth(19)
        .ok_or(SupervisorError::IdentityUnavailable)?;
    Ok((boot.trim().to_owned(), start.to_owned()))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn identity(_pid: u32) -> Result<(String, String), SupervisorError> {
    Err(SupervisorError::IdentityUnavailable)
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
fn stop_socket(root: &Path, attempt: &str) -> PathBuf {
    root.join(format!("{attempt}.stop.sock"))
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

#[cfg(target_os = "macos")]
fn zombie(pid: u32) -> bool {
    let Ok(output) = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    output.status.success() && output.stdout.first() == Some(&b'Z')
}

#[cfg(target_os = "linux")]
fn zombie(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().next())
        == Some("Z")
}

#[cfg(unix)]
fn group_absent(pid: i32) -> bool {
    if unsafe { libc::kill(-pid, 0) } == 0 {
        return false;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
fn matching_child(child: &ChildIdentity) -> bool {
    let Ok(pid) = i32::try_from(child.pid) else {
        return false;
    };
    child.pid != 0
        && child.group_id == child.pid
        && !child.boot_identity.trim().is_empty()
        && !child.start_identity.trim().is_empty()
        && identity(child.pid).ok().as_ref()
            == Some(&(child.boot_identity.clone(), child.start_identity.clone()))
        && unsafe { libc::getpgid(pid) } == pid
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
    let dispatch = store.intent_payload(task_id, attempt_id, "supervisor_dispatch")?;
    let launch = store.intent_payload(task_id, attempt_id, "launch")?;
    if &release != supervisor || dispatch.is_none() || dispatch != launch {
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
    if request[..size] != *expected.as_bytes()
        || persisted != [intended]
        || child.try_wait()?.is_some()
        || identity(child.id()).ok().as_ref() != Some(&(boot.to_owned(), start.to_owned()))
        || unsafe { libc::getpgid(pid) } != pid
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
        if identity(child.id()).ok().as_ref() != Some(&(boot.to_owned(), start.to_owned()))
            || unsafe { libc::getpgid(pid) } != pid
        {
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
fn verify_launch_worktree(
    connection: &Connection,
    plan: &LaunchPlan,
) -> Result<(), SupervisorError> {
    let (selection, intent, identity): (String, String, String) = connection.query_row(
        "SELECT (SELECT payload FROM evidence WHERE task_id=?1 AND kind='selection'),
                (SELECT detail FROM intents WHERE task_id=?1 AND kind='worktree_create'),
                (SELECT payload FROM evidence WHERE task_id=?1 AND kind='worktree_created')",
        [plan.task_id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let selection: SelectionEvidence = serde_json::from_str(&selection)?;
    let record = WorktreeRecord {
        intent: serde_json::from_str::<WorktreeIntent>(&intent)?,
        identity: Some(serde_json::from_str(&identity)?),
    };
    if selection.config_revision != plan.config_revision
        || plan.worktree != plan.expected_worktree.path
        || !worktree::matches_snapshot(
            &plan.expected_worktree,
            &worktree::verify_record(
                &record,
                &selection.candidate.mapping,
                &selection.effective_config.worktree_root,
                &plan.task_id,
            )?,
        )?
    {
        return Err(SupervisorError::Conflict);
    }
    Ok(())
}

#[cfg(unix)]
pub fn worker_gate(plan_path: &Path) -> Result<(), SupervisorError> {
    let plan: LaunchPlan = serde_json::from_slice(&fs::read(plan_path)?)?;
    let mut byte = [0];
    std::io::stdin()
        .read_exact(&mut byte)
        .map_err(|_| SupervisorError::GateClosed)?;
    if byte != *b"R" {
        return Err(SupervisorError::GateClosed);
    }
    worktree::verify_snapshot(&plan.expected_worktree)?;
    let db = plan_path
        .parent()
        .ok_or(SupervisorError::Conflict)?
        .parent()
        .ok_or(SupervisorError::Conflict)?
        .join("state.sqlite3");
    if plan_path
        .parent()
        .is_some_and(|dir| dir.file_name() == Some(std::ffi::OsStr::new("attempts")))
    {
        let connection = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let persisted: String = connection.query_row(
            "SELECT detail FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='launch'",
            params![plan.task_id, plan.attempt_id],
            |row| row.get(0),
        )?;
        if serde_json::from_str::<LaunchPlan>(&persisted)? != plan {
            return Err(SupervisorError::Conflict);
        }
        verify_launch_worktree(&connection, &plan)?;
    }
    let mut command = Command::new(&plan.executable);
    command.args(&plan.args).current_dir(&plan.worktree);
    configure_session(&mut command, &plan.session_environment);
    Err(command.exec().into())
}

#[cfg(unix)]
fn configure_session(command: &mut Command, session: &SessionEnvironment) {
    command.env("HOME", &session.home);
    for (name, value) in [
        ("XDG_CONFIG_HOME", &session.xdg_config_home),
        ("XDG_DATA_HOME", &session.xdg_data_home),
        ("XDG_STATE_HOME", &session.xdg_state_home),
        ("LLXPRT_CONFIG_HOME", &session.llxprt_config_home),
    ] {
        if let Some(value) = value {
            command.env(name, value);
        } else {
            command.env_remove(name);
        }
    }
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
        .arg(&plan_path)
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
            let connection = Connection::open_with_flags(
                store_root
                    .parent()
                    .ok_or(SupervisorError::Conflict)?
                    .join("state.sqlite3"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let registered: i64 = connection.query_row(
                "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='child_registered' AND payload=?3",
                params![plan.task_id, plan.attempt_id, serde_json::to_string(&identity)?],
                |row| row.get(0),
            )?;
            let released: i64 = connection.query_row(
                "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_release'",
                params![plan.task_id, plan.attempt_id], |row| row.get(0),
            )?;
            if registered != 1 || released != 1 {
                return Err(SupervisorError::Conflict);
            }
            verify_launch_worktree(&connection, plan)?;
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
    let pid = child.id();
    let boot_identity = registered.boot_identity.clone();
    let child_start_identity = registered.start_identity.clone();
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (mut out_file, mut err_file) = writers(stdout_file, stderr_file);
    let (sender, receiver) = mpsc::channel();
    let out_sender = sender.clone();
    let out_thread = thread::spawn(move || {
        let result =
            std::io::copy(&mut std::io::BufReader::new(stdout), &mut out_file).and_then(|bytes| {
                out_file.flush()?;
                out_sync.sync_all()?;
                Ok(bytes)
            });
        let _ = out_sender.send(("stdout", result));
    });
    let err_thread = thread::spawn(move || {
        let result =
            std::io::copy(&mut std::io::BufReader::new(stderr), &mut err_file).and_then(|bytes| {
                err_file.flush()?;
                err_sync.sync_all()?;
                Ok(bytes)
            });
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
        child_pid: pid,
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
    write_receipt(store_root, &plan.attempt_id, &receipt)?;
    Ok(status)
}
#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ChildIdentity {
    pub(crate) pid: u32,
    pub(crate) boot_identity: String,
    pub(crate) start_identity: String,
    pub(crate) group_id: u32,
}

#[cfg(unix)]
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: u32,
    pub(crate) boot_identity: String,
    pub(crate) start_identity: String,
}

#[cfg(unix)]
pub(crate) fn recorded_process(payload: &str) -> Option<ProcessIdentity> {
    let value: ProcessIdentity = serde_json::from_str(payload).ok()?;
    (value.pid > 0
        && i32::try_from(value.pid).is_ok()
        && !value.boot_identity.trim().is_empty()
        && !value.start_identity.trim().is_empty())
    .then_some(value)
}

/// Proves the registered direct processes and their process groups are absent.
/// Recorded known descendants are checked individually. This does not prove
/// that a deliberately untracked descendant escaped into another group or
/// session is absent; that remains an out-of-scope cooperative constraint.
#[cfg(unix)]
pub(crate) fn registered_processes_absent(
    child: &ChildIdentity,
    supervisor: &ProcessIdentity,
    tracked: &[ProcessIdentity],
) -> bool {
    let bounded = |boot: &str, start: &str| {
        !boot.trim().is_empty()
            && boot.len() <= 256
            && !start.trim().is_empty()
            && start.len() <= 64
    };
    if child.pid == 0
        || supervisor.pid == 0
        || i32::try_from(child.pid).is_err()
        || i32::try_from(supervisor.pid).is_err()
        || child.pid == supervisor.pid
        || child.group_id != child.pid
        || !bounded(&child.boot_identity, &child.start_identity)
        || !bounded(&supervisor.boot_identity, &supervisor.start_identity)
        || tracked.iter().any(|process| {
            process.pid == 0
                || i32::try_from(process.pid).is_err()
                || !bounded(&process.boot_identity, &process.start_identity)
                || process.boot_identity != child.boot_identity
        })
    {
        return false;
    }
    let Ok(current) = identity(std::process::id()) else {
        return false;
    };
    if current.0 != child.boot_identity || current.0 != supervisor.boot_identity {
        return false;
    }
    let pid_absent = |pid: u32| {
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            return false;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    };
    pid_absent(child.pid)
        && pid_absent(supervisor.pid)
        && tracked.iter().all(|process| pid_absent(process.pid))
        && group_absent(child.group_id as i32)
        && group_absent(supervisor.pid as i32)
}

#[cfg(unix)]
pub(crate) fn verified_live_process(child: &ChildIdentity, supervisor: &ProcessIdentity) -> bool {
    let bounded = |boot: &str, start: &str| {
        !boot.trim().is_empty()
            && boot.len() <= 256
            && !start.trim().is_empty()
            && start.len() <= 64
    };
    child.pid != supervisor.pid
        && bounded(&child.boot_identity, &child.start_identity)
        && bounded(&supervisor.boot_identity, &supervisor.start_identity)
        && matching_child(child)
        && !zombie(child.pid)
        && identity(supervisor.pid).ok().as_ref()
            == Some(&(
                supervisor.boot_identity.clone(),
                supervisor.start_identity.clone(),
            ))
        && !zombie(supervisor.pid)
        && unsafe { libc::getpgid(supervisor.pid as i32) } == supervisor.pid as i32
}

#[cfg(unix)]
fn private_bytes(path: &Path) -> Option<Vec<u8>> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return None;
    }
    fs::read(path).ok()
}

#[cfg(unix)]
fn private_log_size(path: &Path) -> Option<u64> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path).ok()?;
    (metadata.file_type().is_file() && metadata.permissions().mode() & 0o077 == 0)
        .then_some(metadata.len())
}

/// Only an exact durable exit with a proven absent process group releases capacity.
#[cfg(unix)]
pub fn reconcile_attempt(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<Reconciliation, SupervisorError> {
    if !valid_attempt(attempt_id) {
        return Err(SupervisorError::Conflict);
    }
    let held = |reason: &str| Reconciliation::Held {
        reason: reason.into(),
    };
    let attempts = store.root().join("attempts");
    use std::os::unix::fs::PermissionsExt;
    let Ok(dir) = fs::symlink_metadata(&attempts) else {
        return Ok(held("missing attempts directory"));
    };
    if !dir.file_type().is_dir() || dir.permissions().mode() & 0o777 != 0o700 {
        return Ok(held("unsafe attempts directory"));
    }
    let plan: LaunchPlan = match private_bytes(&attempts.join(format!("{attempt_id}.plan.json")))
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(plan) => plan,
        None => return Ok(held("missing or invalid plan")),
    };
    if plan.task_id != task_id
        || plan.attempt_id != attempt_id
        || plan.session_id != task_id
        || plan.config_revision.is_empty()
    {
        return Ok(held("plan identity mismatch"));
    }
    let Some(persisted) = store.intent_payload(task_id, attempt_id, "launch")? else {
        return Ok(held("missing launch intent"));
    };
    if serde_json::from_str::<LaunchPlan>(&persisted).ok().as_ref() != Some(&plan) {
        return Ok(held("launch intent mismatch"));
    }
    let Some(dispatch) = store.intent_payload(task_id, attempt_id, "supervisor_dispatch")? else {
        return Ok(held("missing dispatch intent"));
    };
    if serde_json::from_str::<LaunchPlan>(&dispatch).ok().as_ref() != Some(&plan) {
        return Ok(held("dispatch plan mismatch"));
    }
    let Some(worktree) = store.evidence_payload(task_id, None, "worktree_created")? else {
        return Ok(held("missing worktree evidence"));
    };
    let Some(worktree) = serde_json::from_str::<WorktreeIdentity>(&worktree).ok() else {
        return Ok(held("invalid worktree evidence"));
    };
    let record = store.worktree_record(task_id)?;
    if worktree.path != plan.worktree
        || record.as_ref().is_none_or(|r| {
            r.identity.as_ref() != Some(&worktree)
                || r.intent.path != worktree.path
                || r.intent.branch != worktree.branch
                || r.intent.base != worktree.base
                || r.intent.repository != worktree.repository
        })
    {
        return Ok(held("worktree identity mismatch"));
    }
    let Some(selection) = store.selection_evidence(task_id)? else {
        return Ok(held("missing selection"));
    };
    if selection.config_revision != plan.config_revision
        || selection.candidate.mapping.code_repository != worktree.repository
    {
        return Ok(held("selection mismatch"));
    }
    let Some(claim) = store.evidence_payload(task_id, None, "claim_verified")? else {
        return Ok(held("missing claim evidence"));
    };
    if claim.trim().is_empty() || claim != selection.effective_config.assignment_login {
        return Ok(held("claim identity mismatch"));
    }
    let Some(child_file) = private_bytes(&attempts.join(format!("{attempt_id}.child.json")))
        .and_then(|bytes| serde_json::from_slice::<ChildIdentity>(&bytes).ok())
    else {
        return Ok(held("missing or invalid child identity"));
    };
    let Some(child_evidence) =
        store.evidence_payload(task_id, Some(attempt_id), "child_registered")?
    else {
        return Ok(held("missing child registration"));
    };
    if serde_json::from_str::<ChildIdentity>(&child_evidence)
        .ok()
        .as_ref()
        != Some(&child_file)
        || child_file.pid == 0
        || child_file.group_id != child_file.pid
        || child_file.boot_identity.is_empty()
        || child_file.start_identity.is_empty()
    {
        return Ok(held("child registration mismatch"));
    }
    if store
        .evidence_payload(task_id, Some(attempt_id), "log_failure")?
        .is_some()
    {
        return Ok(held("log drain failed"));
    }
    let Some(release) = store.intent_payload(task_id, attempt_id, "gate_release")? else {
        return Ok(held("missing gate release decision"));
    };
    let Some(ready) = store.evidence_payload(task_id, Some(attempt_id), "supervisor_ready")? else {
        return Ok(held("missing or invalid supervisor identity"));
    };
    let Some(supervisor) = recorded_process(&ready) else {
        return Ok(held("missing or invalid supervisor identity"));
    };
    if recorded_process(&release).as_ref() != Some(&supervisor) {
        return Ok(held("supervisor identity contradiction"));
    }
    let sent = store.evidence_payload(task_id, Some(attempt_id), "gate_sent")?;
    if sent.is_some() && sent.as_deref().and_then(recorded_process).as_ref() != Some(&supervisor) {
        return Ok(held("supervisor identity contradiction"));
    }
    if supervisor.pid == child_file.pid {
        return Ok(held("supervisor and child identity contradiction"));
    }
    let receipt_path = attempts.join(format!("{attempt_id}.receipt.json"));
    if !receipt_path.exists() {
        if sent.is_none()
            || !store.active_attempt_reservation(task_id, attempt_id)?
            || store.stop_intent(task_id, attempt_id)?.is_some()
            || attempts
                .join(format!("{attempt_id}.supervisor-error.json"))
                .exists()
        {
            return Ok(held("live worker identity or reservation unverified"));
        }
        let tracked = store
            .evidence_payloads(task_id, attempt_id, "tracked_descendant")?
            .into_iter()
            .map(|payload| recorded_process(&payload))
            .collect::<Option<Vec<_>>>();
        let Some(tracked) = tracked else {
            return Ok(held("invalid tracked descendant identity"));
        };
        if registered_processes_absent(&child_file, &supervisor, &tracked) {
            return Ok(held(
                "receipt missing; registered processes absent; operator recovery required",
            ));
        }
        if !verified_live_process(&child_file, &supervisor) {
            return Ok(held("live worker identity or reservation unverified"));
        }
        return Ok(Reconciliation::Running);
    }
    let receipt: ExitReceipt =
        match private_bytes(&receipt_path).and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
            Some(receipt) => receipt,
            None => return Ok(held("missing or invalid receipt")),
        };
    let stdout = attempts.join(format!("{attempt_id}.stdout.log"));
    let stderr = attempts.join(format!("{attempt_id}.stderr.log"));
    if receipt.attempt_id != attempt_id
        || receipt.child_pid != child_file.pid
        || receipt.boot_identity != child_file.boot_identity
        || receipt.child_start_identity != child_file.start_identity
        || receipt.child_pid == 0
        || i32::try_from(receipt.child_pid).is_err()
        || receipt.boot_identity.trim().is_empty()
        || receipt.child_start_identity.trim().is_empty()
        || receipt.exit_code.is_some() == receipt.signal.is_some()
        || receipt.stdout_path != stdout
        || receipt.stderr_path != stderr
    {
        return Ok(held("receipt identity or shape mismatch"));
    }
    if private_log_size(&stdout) != Some(receipt.stdout_bytes)
        || private_log_size(&stderr) != Some(receipt.stderr_bytes)
    {
        return Ok(held("missing, unsafe or incomplete logs"));
    }
    let evidence = serde_json::to_string(&receipt)?;
    let outcome = format!(
        "exit_code={:?};signal={:?}",
        receipt.exit_code, receipt.signal
    );
    let completed = Reconciliation::Completed {
        exit_code: receipt.exit_code,
        signal: receipt.signal,
    };
    if store.reconciled_exit(task_id, attempt_id, &evidence, &outcome)? {
        return Ok(completed);
    }
    match identity(supervisor.pid) {
        Ok((boot, start))
            if boot != supervisor.boot_identity || start != supervisor.start_identity =>
        {
            return Ok(held("supervisor identity mismatch"));
        }
        Err(_) => {
            let supervisor_group =
                i32::try_from(supervisor.pid).map_err(|_| SupervisorError::IdentityUnavailable)?;
            if !group_absent(supervisor_group) {
                // Darwin retains detached supervisors as zombies until their
                // parent reaps them; a zombie cannot launch or control a worker.
                #[cfg(target_os = "macos")]
                if zombie(supervisor.pid) {
                    // Child group absence is checked separately below.
                } else {
                    return Ok(held("supervisor identity unavailable"));
                }
                #[cfg(not(target_os = "macos"))]
                return Ok(held("supervisor identity unavailable"));
            }
        }
        Ok(_) => {}
    }
    if let Ok((boot, start)) = identity(receipt.child_pid)
        && (boot != receipt.boot_identity || start != receipt.child_start_identity)
    {
        return Ok(held("child identity mismatch"));
    }
    let pgid =
        i32::try_from(receipt.child_pid).map_err(|_| SupervisorError::IdentityUnavailable)?;
    // The negative argument probes the whole child group, never a bare PID.
    if unsafe { libc::kill(-pgid, 0) } == 0 {
        return Ok(held("child process group is alive"));
    }
    if std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Ok(held("child process group absence is unproven"));
    }
    store.reconcile_verified_exit(task_id, attempt_id, &evidence, &outcome)?;
    Ok(completed)
}

#[cfg(not(unix))]
pub fn reconcile_attempt(
    _store: &mut StateStore,
    _task_id: &str,
    _attempt_id: &str,
) -> Result<Reconciliation, SupervisorError> {
    Err(SupervisorError::ExecutionUnavailable)
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

fn private_attempts(root: &Path) -> Result<PathBuf, SupervisorError> {
    let path = root.join("attempts");
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(&path)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(&path)?;
        File::open(root)?.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&path)?.permissions().mode() & 0o777 != 0o700 {
            return Err(SupervisorError::Conflict);
        }
    }
    Ok(path)
}

fn valid_attempt(attempt: &str) -> bool {
    !attempt.is_empty()
        && attempt.len() <= 128
        && attempt
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), SupervisorError> {
    let mut file = private_file(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    File::open(path.parent().ok_or(SupervisorError::Conflict)?)?.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn write_private_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), SupervisorError> {
    let temp = path.with_extension("child.tmp");
    let mut file = private_file(&temp)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    File::open(path.parent().ok_or(SupervisorError::Conflict)?)?.sync_all()?;
    Ok(())
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
        let (persisted, worktree) = row.ok_or(SupervisorError::Conflict)?;
        if serde_json::from_str::<LaunchPlan>(&persisted)? != plan {
            return Err(SupervisorError::Conflict);
        }
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

/// Production always starts this binary. The explicit binary variant permits an
/// integration test process to dispatch the built CLI instead of its test harness.
#[cfg(unix)]
pub fn execute(store: &mut StateStore, plan: &LaunchPlan) -> Result<(), SupervisorError> {
    execute_with_binary(store, plan, &env::current_exe()?)
}

#[cfg(unix)]
pub fn execute_with_binary(
    store: &mut StateStore,
    plan: &LaunchPlan,
    binary: &Path,
) -> Result<(), SupervisorError> {
    if !valid_attempt(&plan.attempt_id) {
        return Err(SupervisorError::Conflict);
    }
    let root = store.root().to_path_buf();
    let attempts = private_attempts(&root)?;
    let serialized = serde_json::to_string(plan)?;
    // A second dispatch cannot overwrite the plan or launch the worker.
    write_private_json(
        &attempts.join(format!("{}.plan.json", plan.attempt_id)),
        plan,
    )?;
    store.begin_supervision(&plan.task_id, &plan.attempt_id, &serialized)?;
    let stderr = private_file(&attempts.join(format!("{}.supervisor.log", plan.attempt_id)))?;
    let mut command = Command::new(binary);
    command
        .arg("__supervise")
        .arg(&root)
        .arg(&plan.attempt_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr);
    use std::os::unix::process::CommandExt;
    // This process must outlive the coordinator without inheriting its terminal/session.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let mut ready = String::new();
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
    if reader.read_line(&mut ready)? != 6 || ready != "READY\n" {
        return Err(SupervisorError::ExecutionUnavailable);
    }
    let (boot, start) = identity(child.id())?;
    let process = serde_json::json!({"pid":child.id(),"boot_identity":boot,"start_identity":start});
    let registered: ChildIdentity =
        private_bytes(&attempts.join(format!("{}.child.json", plan.attempt_id)))
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(SupervisorError::IdentityUnavailable)?;
    let child_pid =
        i32::try_from(registered.pid).map_err(|_| SupervisorError::IdentityUnavailable)?;
    if registered.pid == child.id()
        || registered.pid != registered.group_id
        || unsafe { libc::getpgid(child_pid) } != child_pid
        || identity(registered.pid).ok().as_ref()
            != Some(&(
                registered.boot_identity.clone(),
                registered.start_identity.clone(),
            ))
    {
        return Err(SupervisorError::IdentityUnavailable);
    }
    store.record_evidence(
        &plan.task_id,
        Some(&plan.attempt_id),
        "child_registered",
        &serde_json::to_string(&registered)?,
    )?;
    store.record_evidence(
        &plan.task_id,
        Some(&plan.attempt_id),
        "supervisor_ready",
        &process.to_string(),
    )?;
    store.record_intent(
        &format!("gate-{}", plan.attempt_id),
        &plan.task_id,
        Some(&plan.attempt_id),
        "gate_release",
        &process.to_string(),
    )?;
    let mut gate = child.stdin.take().expect("piped stdin");
    gate.write_all(b"R")?;
    gate.flush()?;
    store.record_evidence(
        &plan.task_id,
        Some(&plan.attempt_id),
        "gate_sent",
        &process.to_string(),
    )?;
    Ok(())
}

#[cfg(not(unix))]
pub fn execute(_store: &mut StateStore, _plan: &LaunchPlan) -> Result<(), SupervisorError> {
    Err(SupervisorError::ExecutionUnavailable)
}
