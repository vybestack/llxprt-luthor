use super::{
    error::SupervisorError,
    processes::{identity, private_bytes, valid_attempt},
    readiness,
    storage::{private_attempts, private_file, validate_stop_socket_path, write_private_json},
    worker::verify_launch_worktree,
};
use crate::model::{ChildIdentity, LaunchPlan};
use crate::state::{journal, launches};
use crate::{
    config::Config,
    state::{NeverDispatchedContext, StateStore},
};
use std::{
    env,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

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
    let serialized =
        launches::launch_intent(store, &plan.attempt_id)?.ok_or(SupervisorError::Conflict)?;
    if serde_json::from_str::<LaunchPlan>(&serialized)? != *plan {
        return Err(SupervisorError::Conflict);
    }
    let root = store.root().to_path_buf();
    let attempts = private_attempts(&root)?;
    // A second dispatch cannot overwrite the plan or launch the worker.
    write_private_json(
        &attempts.join(format!("{}.plan.json", plan.attempt_id)),
        plan,
    )?;
    launches::begin_supervision(store, &plan.task_id, &plan.attempt_id, &serialized)?;
    launch_dispatched(store, plan, binary)
}

/// Explicit amended launch. The original launch intent is never rewritten.
#[cfg(unix)]
pub fn execute_amended(
    store: &mut StateStore,
    context: &NeverDispatchedContext,
    config: &Config,
    revision: &str,
) -> Result<(), SupervisorError> {
    execute_amended_with_binary(store, context, config, revision, &env::current_exe()?)
}

#[cfg(unix)]
pub fn execute_amended_with_binary(
    store: &mut StateStore,
    context: &NeverDispatchedContext,
    config: &Config,
    revision: &str,
    binary: &Path,
) -> Result<(), SupervisorError> {
    let proof = store.amended_dispatch_proof(context, config, revision)?;
    let plan = &proof.effective_plan;
    validate_stop_socket_path(store.root(), &plan.attempt_id)?;
    if super::inspect_never_dispatched_storage(store.root(), &plan.attempt_id)?
        || !plan.session_environment.matches_current()?
    {
        return Err(SupervisorError::Conflict);
    }
    verify_launch_worktree(&store.connection, plan)?;
    let attempts = private_attempts(store.root())?;
    let committed = store.begin_amended_supervision(context, config, revision)?;
    write_private_json(
        &attempts.join(format!("{}.plan.json", plan.attempt_id)),
        &committed.effective_plan,
    )?;
    launch_dispatched(store, &committed.effective_plan, binary)
}

#[cfg(unix)]
fn launch_dispatched(
    store: &mut StateStore,
    plan: &LaunchPlan,
    binary: &Path,
) -> Result<(), SupervisorError> {
    let root = store.root().to_path_buf();
    let attempts = private_attempts(&root)?;
    super::binding::verify_worker_plan(&store.connection, &root, plan)?;
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
    let mut gate = child.stdin.take().expect("piped stdin");
    let supervisor_pid = child.id();
    let stdout = child.stdout.take().expect("piped stdout");
    std::thread::Builder::new()
        .name(format!("supervisor-reaper-{supervisor_pid}"))
        .spawn(move || {
            let mut child = child;
            let _ = child.wait();
        })?;
    let mut reader = stdout;
    readiness::wait_for_ready(&mut reader)?;
    release_ready_worker(store, plan, supervisor_pid, &mut gate)
}

#[cfg(unix)]
fn release_ready_worker(
    store: &mut StateStore,
    plan: &LaunchPlan,
    supervisor_pid: u32,
    gate: &mut impl Write,
) -> Result<(), SupervisorError> {
    super::binding::verify_worker_plan(&store.connection, store.root(), plan)?;
    verify_launch_worktree(&store.connection, plan)?;
    let attempts = store.root().join("attempts");
    let (boot, start) = identity(supervisor_pid)?;
    let process =
        serde_json::json!({"pid":supervisor_pid,"boot_identity":boot,"start_identity":start});
    let registered: ChildIdentity =
        private_bytes(&attempts.join(format!("{}.child.json", plan.attempt_id)))
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(SupervisorError::IdentityUnavailable)?;
    let child_pid =
        i32::try_from(registered.pid).map_err(|_| SupervisorError::IdentityUnavailable)?;
    if registered.pid == supervisor_pid
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
    journal::record_evidence(
        store,
        &plan.task_id,
        Some(&plan.attempt_id),
        "child_registered",
        &serde_json::to_string(&registered)?,
    )?;
    journal::record_evidence(
        store,
        &plan.task_id,
        Some(&plan.attempt_id),
        "supervisor_ready",
        &process.to_string(),
    )?;
    journal::record_intent(
        store,
        &format!("gate-{}", plan.attempt_id),
        &plan.task_id,
        Some(&plan.attempt_id),
        "gate_release",
        &process.to_string(),
    )?;
    gate.write_all(b"R")?;
    gate.flush()?;
    journal::record_evidence(
        store,
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
