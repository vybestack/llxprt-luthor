use super::ports::{ContinuationLocalInspector, ContinuationRefusal as Refusal};
use crate::{
    state::{NeverDispatchedContext, WorktreeRecord},
    supervisor::{self, SessionEnvironment},
    worktree,
};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

pub struct OsContinuationLocalInspector;

impl OsContinuationLocalInspector {
    pub fn verify_worktree(&mut self, context: &NeverDispatchedContext) -> Result<(), Refusal> {
        let selection = context.selection();
        let record = WorktreeRecord {
            intent: context.worktree_intent().clone(),
            identity: Some(context.worktree_identity().clone()),
        };
        worktree::verify_never_dispatched(
            &record,
            &selection.candidate.mapping,
            &selection.effective_config.worktree_root,
            context.task_id(),
        )
        .map_err(|_| Refusal::WorktreeChanged)
    }

    pub fn inspect_artifacts(&mut self, context: &NeverDispatchedContext) -> Result<(), Refusal> {
        let root = &context.selection().effective_config.state_root;
        supervisor::validate_stop_socket_path(root, context.attempt_id())
            .map_err(|_| Refusal::StorageUnavailable)?;
        let present = supervisor::inspect_never_dispatched_storage(root, context.attempt_id())
            .map_err(|_| Refusal::StorageUnavailable)?;
        if present {
            return Err(Refusal::ArtifactConflict);
        }
        Ok(())
    }
}

impl ContinuationLocalInspector for OsContinuationLocalInspector {
    fn session_environment(&mut self) -> Result<SessionEnvironment, Refusal> {
        SessionEnvironment::capture().map_err(|_| Refusal::EnvironmentUnavailable)
    }

    fn inspect(&mut self, context: &NeverDispatchedContext) -> Result<(), Refusal> {
        verify_executable(&context.plan().executable, &context.plan().worktree)?;
        self.verify_worktree(context)?;
        self.inspect_artifacts(context)
    }
}

fn executable_paths(executable: &Path, cwd: &Path) -> Result<Vec<PathBuf>, Refusal> {
    if executable.is_absolute() {
        return Ok(vec![executable.to_owned()]);
    }
    if executable.components().count() > 1 {
        return Ok(vec![cwd.join(executable)]);
    }
    let path = env::var_os("PATH").ok_or(Refusal::ExecutableUnavailable)?;
    Ok(env::split_paths(&path)
        .map(|dir| {
            let dir = if dir.is_absolute() {
                dir
            } else {
                cwd.join(dir)
            };
            dir.join(executable)
        })
        .collect())
}

fn verify_executable(executable: &Path, cwd: &Path) -> Result<(), Refusal> {
    let paths = executable_paths(executable, cwd)?;
    for path in paths {
        if executable_file(&path) {
            return Ok(());
        }
    }
    Err(Refusal::ExecutableUnavailable)
}

#[cfg(unix)]
fn executable_file(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path_text) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
        && unsafe { libc::access(path_text.as_ptr(), libc::X_OK) } == 0
}

#[cfg(not(unix))]
fn executable_file(_: &Path) -> bool {
    false
}
