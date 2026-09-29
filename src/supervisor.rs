//! Launch planning and the Unix gated child runner.
use crate::{
    config::{ConfigError, RenderedCommand, TaskValues},
    state::{StateError, StateStore},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    State(#[from] StateError),
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

/// Waits for one explicit `R` byte before starting the worker. EOF keeps the
/// already-reserved attempt held and launches nothing. Unix process groups are
/// used so later reconciliation can independently prove group termination.
#[cfg(unix)]
pub fn run_gated_child<R: Read>(
    plan: &LaunchPlan,
    mut gate: R,
    store_root: &Path,
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
    let status = child.wait()?;
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
    };
    write_receipt(store_root, &plan.attempt_id, &receipt)?;
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

/// Detached supervisor wiring is not yet safe; callers must not bypass this gate.
pub fn execute(_plan: &LaunchPlan) -> Result<(), SupervisorError> {
    Err(SupervisorError::ExecutionUnavailable)
}
