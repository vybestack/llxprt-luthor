//! Launch planning and the Unix gated child runner.
use crate::{
    config::{ConfigError, RenderedCommand, TaskValues},
    state::{StateError, StateStore, WorktreeIdentity},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
#[cfg(unix)]
use std::{
    os::unix::net::{UnixListener, UnixStream},
    process::Child,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    State(#[from] StateError),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub task_id: String,
    pub attempt_id: String,
    pub session_id: String,
    pub worktree: PathBuf,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub config_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
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

/// Renders one initial attempt; the caller remains responsible for fresh claim
/// and absent-PR evidence. This function deliberately cannot start a worker.
pub fn prepare_initial(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<LaunchPlan, SupervisorError> {
    let selection = store.claimed_worktree_context(task_id)?;
    let record = store
        .worktree_record(task_id)?
        .ok_or(SupervisorError::Conflict)?;
    let identity = record.identity.ok_or(SupervisorError::Conflict)?;
    if identity.path != record.intent.path
        || identity.repository != selection.candidate.mapping.code_repository
        || identity.path != fs::canonicalize(&identity.path)?
    {
        return Err(SupervisorError::Conflict);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(&identity.path)?;
        if metadata.dev() != identity.device || metadata.ino() != identity.inode {
            return Err(SupervisorError::Conflict);
        }
    }
    let values = TaskValues {
        task_issue_number: selection.candidate.issue_number.to_string(),
        task_repository: selection.candidate.repository.clone(),
        task_issue_url: selection.candidate.issue_url.clone(),
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        worktree: identity.path.to_string_lossy().into_owned(),
    };
    let RenderedCommand { executable, args } =
        selection.effective_config.initial.render(&values)?;
    let worktree = identity.path;
    let cwd = worktree.to_str().ok_or(SupervisorError::Conflict)?;
    if !requires_pair(&args, "--session", task_id)
        || !requires_pair(&args, "--cwd", cwd)
        || prompt(&args).is_none_or(str::is_empty)
    {
        return Err(SupervisorError::Conflict);
    }
    let plan = LaunchPlan {
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        session_id: task_id.to_owned(),
        worktree,
        executable,
        args,
        config_revision: selection.config_revision,
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
    let selection = store
        .selection_evidence(task_id)?
        .ok_or(SupervisorError::Conflict)?;
    let record = store
        .worktree_record(task_id)?
        .ok_or(SupervisorError::Conflict)?;
    let identity = record.identity.ok_or(SupervisorError::Conflict)?;
    if first.task_id != task_id
        || latest.task_id != task_id
        || first.session_id != task_id
        || latest.session_id != task_id
        || first.worktree != identity.path
        || latest.worktree != identity.path
        || first.config_revision != selection.config_revision
        || latest.config_revision != selection.config_revision
        || identity.path != record.intent.path
        || identity.repository != selection.candidate.mapping.code_repository
        || identity.path != fs::canonicalize(&identity.path)?
    {
        return Err(SupervisorError::Conflict);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(&identity.path)?;
        if metadata.dev() != identity.device || metadata.ino() != identity.inode {
            return Err(SupervisorError::Conflict);
        }
    }
    let worktree = identity.path;
    let cwd = worktree.to_str().ok_or(SupervisorError::Conflict)?;
    let values = TaskValues {
        task_issue_number: selection.candidate.issue_number.to_string(),
        task_repository: selection.candidate.repository.clone(),
        task_issue_url: selection.candidate.issue_url.clone(),
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        worktree: cwd.to_owned(),
    };
    let RenderedCommand { executable, args } = selection.effective_config.resume.render(&values)?;
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
    let plan = LaunchPlan {
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        session_id: task_id.to_owned(),
        worktree,
        executable,
        args,
        config_revision: selection.config_revision,
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
    let output = Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()?;
    if !output.status.success() {
        return Err(SupervisorError::IdentityUnavailable);
    }
    let start = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if start.is_empty() {
        return Err(SupervisorError::IdentityUnavailable);
    }
    Ok((boot, start))
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

/// Persist the request before contacting the supervisor. The coordinator never signals a PID.
#[cfg(unix)]
pub fn request_stop(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<(), SupervisorError> {
    if !valid_attempt(attempt_id) {
        return Err(SupervisorError::Conflict);
    }
    store.record_stop_intent(task_id, attempt_id)?;
    let recorded = store
        .evidence_payload(task_id, Some(attempt_id), "supervisor_ready")?
        .as_deref()
        .and_then(recorded_process)
        .ok_or(SupervisorError::StopUnavailable)?;
    if identity(recorded.pid).ok().as_ref()
        != Some(&(recorded.boot_identity, recorded.start_identity))
    {
        return Err(SupervisorError::StopUnavailable);
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
    // The unreaped Child owns this PID. Signal its verified dedicated group only.
    if unsafe { libc::kill(-pid, libc::SIGTERM) } != 0 {
        stream.write_all(b"N")?;
        return Ok(());
    }
    signals.push(libc::SIGTERM);
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            stream.write_all(b"Y")?;
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    if child.try_wait()?.is_none()
        && identity(child.id()).ok().as_ref() == Some(&(boot.to_owned(), start.to_owned()))
        && unsafe { libc::getpgid(pid) } == pid
    {
        if unsafe { libc::kill(-pid, libc::SIGKILL) } != 0 {
            stream.write_all(b"N")?;
            return Ok(());
        }
        signals.push(libc::SIGKILL);
    }
    stream.write_all(b"Y")?;
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
    run_gated_child_control(plan, gate, store_root, None)
}

#[cfg(unix)]
fn run_gated_child_control<R: Read>(
    plan: &LaunchPlan,
    mut gate: R,
    store_root: &Path,
    control: Option<&UnixListener>,
) -> Result<ExitStatus, SupervisorError> {
    let mut release = [0u8; 1];
    if gate.read(&mut release)? != 1 || release[0] != b'R' {
        return Err(SupervisorError::GateClosed);
    }
    if plan.attempt_id.is_empty() || plan.attempt_id.contains('/') || plan.attempt_id.contains('\\')
    {
        return Err(SupervisorError::Conflict);
    }
    fs::create_dir_all(store_root)?;
    let stdout_path = store_root.join(format!("{}.stdout.log", plan.attempt_id));
    let stderr_path = store_root.join(format!("{}.stderr.log", plan.attempt_id));
    let stdout_file = private_file(&stdout_path)?;
    let stderr_file = private_file(&stderr_path)?;
    let mut command = Command::new(&plan.executable);
    command
        .args(&plan.args)
        .current_dir(&plan.worktree)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Unix-only: a dedicated process group is required for independent reconciliation.
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id();
    let (boot_identity, child_start_identity) = identity(pid)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out_thread = thread::spawn(move || -> Result<u64, std::io::Error> {
        let mut file = stdout_file;
        let bytes = std::io::copy(&mut std::io::BufReader::new(stdout), &mut file)?;
        file.sync_all()?;
        Ok(bytes)
    });
    let err_thread = thread::spawn(move || -> Result<u64, std::io::Error> {
        let mut file = stderr_file;
        let bytes = std::io::copy(&mut std::io::BufReader::new(stderr), &mut file)?;
        file.sync_all()?;
        Ok(bytes)
    });
    let mut stop_signals = Vec::new();
    let status = if let Some(listener) = control {
        loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
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
            thread::sleep(Duration::from_millis(20));
        }
    } else {
        child.wait()?
    };
    let stdout_bytes = out_thread
        .join()
        .map_err(|_| SupervisorError::ExecutionUnavailable)??;
    let stderr_bytes = err_thread
        .join()
        .map_err(|_| SupervisorError::ExecutionUnavailable)??;
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
#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    boot_identity: String,
    start_identity: String,
}

#[cfg(unix)]
fn recorded_process(payload: &str) -> Option<ProcessIdentity> {
    let value: ProcessIdentity = serde_json::from_str(payload).ok()?;
    (value.pid > 0
        && i32::try_from(value.pid).is_ok()
        && !value.boot_identity.trim().is_empty()
        && !value.start_identity.trim().is_empty())
    .then_some(value)
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
    let receipt: ExitReceipt =
        match private_bytes(&attempts.join(format!("{attempt_id}.receipt.json")))
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        {
            Some(receipt) => receipt,
            None => return Ok(held("missing or invalid receipt")),
        };
    let stdout = attempts.join(format!("{attempt_id}.stdout.log"));
    let stderr = attempts.join(format!("{attempt_id}.stderr.log"));
    if receipt.attempt_id != attempt_id
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
    let mut supervisor = None;
    for (evidence, kind) in [
        (true, "supervisor_ready"),
        (false, "gate_release"),
        (true, "gate_sent"),
    ] {
        let payload = if evidence {
            store.evidence_payload(task_id, Some(attempt_id), kind)?
        } else {
            store.intent_payload(task_id, attempt_id, kind)?
        };
        let Some(process) = payload.as_deref().and_then(recorded_process) else {
            return Ok(held("missing or invalid supervisor identity"));
        };
        if supervisor.as_ref().is_some_and(|prior| prior != &process) {
            return Ok(held("supervisor identity contradiction"));
        }
        supervisor = Some(process);
    }
    let supervisor = supervisor.expect("three verified process identities");
    if supervisor.pid == receipt.child_pid {
        return Ok(held("supervisor and child identity contradiction"));
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
            if unsafe { libc::kill(-supervisor_group, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            {
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
        if identity.path != plan.worktree || fs::canonicalize(&plan.worktree)? != plan.worktree {
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
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(b"READY\n")?;
        stdout.flush()?;
        drop(stdout);
        let mut gate = [0u8; 1];
        if std::io::stdin().read(&mut gate)? != 1 || gate[0] != b'R' {
            return Err(SupervisorError::GateClosed);
        }
        let release: i64 = connection.query_row(
            "SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND task_id=?2 AND kind='gate_release'",
            params![attempt, plan.task_id], |row| row.get(0)
        )?;
        if release != 1 {
            return Err(SupervisorError::Conflict);
        }
        #[cfg(unix)]
        run_gated_child_control(
            &plan,
            std::io::Cursor::new(gate),
            &attempts,
            Some(&listener),
        )?;
        #[cfg(not(unix))]
        run_gated_child(&plan, std::io::Cursor::new(gate), &attempts)?;
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
