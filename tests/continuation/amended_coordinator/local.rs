use super::{Case, Refusal};
use crate::continuation::{Local, git};
use luthor::{
    coordinator::ContinuationLocalInspector, state::NeverDispatchedContext,
    supervisor::SessionEnvironment,
};
use std::fs;

pub(super) struct Checks<'a> {
    pub inner: &'a mut Local,
    pub mutation: Option<(&'static str, usize)>,
    pub audit_seen: &'a mut Vec<bool>,
}
impl ContinuationLocalInspector for Checks<'_> {
    fn session_environment(&mut self) -> Result<SessionEnvironment, Refusal> {
        let mut environment = self.inner.session_environment()?;
        if self.mutation == Some(("environment", self.inner.inspections + 1)) {
            environment.home = "/other".into();
        }
        Ok(environment)
    }
    fn inspect(&mut self, context: &NeverDispatchedContext) -> Result<(), Refusal> {
        self.audit_seen.push(context.amendment().is_some());
        if let Some((mutation, when)) = self.mutation
            && when == self.inner.inspections + 1
        {
            mutate(context, mutation);
        }
        self.inner.inspect(context)
    }
}
fn mutate(context: &NeverDispatchedContext, mutation: &str) {
    let worktree = &context.plan().worktree;
    match mutation {
        "head" => git(worktree, &["commit", "--allow-empty", "-m", "drift"]),
        "branch" => git(worktree, &["checkout", "-b", "other"]),
        "dirty" => fs::write(worktree.join("README"), "drift").unwrap(),
        "path" => fs::rename(worktree, worktree.with_extension("moved")).unwrap(),
        "artifact" => fs::write(
            context
                .selection()
                .effective_config
                .state_root
                .join("attempts/attempt-task-a.unknown"),
            "conflict",
        )
        .unwrap(),
        "storage" => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    context
                        .selection()
                        .effective_config
                        .state_root
                        .join("attempts"),
                    fs::Permissions::from_mode(0o755),
                )
                .unwrap();
            }
        }
        "history" => {
            let db = rusqlite::Connection::open(
                context
                    .selection()
                    .effective_config
                    .state_root
                    .join("state.sqlite3"),
            )
            .unwrap();
            db.execute_batch("INSERT INTO evidence(task_id,kind,payload) VALUES('task-a','held_reason','external drift');").unwrap();
        }
        _ => unreachable!(),
    }
}

#[test]
fn amended_coordinator_real_worktree_path_head_branch_and_namespace_drift_stop_both_rounds() {
    for round in [1, 2] {
        for (mutation, reason) in [
            ("head", Refusal::WorktreeChanged),
            ("branch", Refusal::WorktreeChanged),
            ("path", Refusal::WorktreeChanged),
            ("dirty", Refusal::WorktreeChanged),
            ("artifact", Refusal::ArtifactConflict),
            ("environment", Refusal::EnvironmentChanged),
        ] {
            let mut case = Case::new();
            case.local_mutation = Some((mutation, round));
            case.held(reason, round - 1);
        }
    }
}

#[test]
#[cfg(unix)]
fn amended_coordinator_private_storage_permissions_are_required_in_both_rounds() {
    for round in [1, 2] {
        let mut case = Case::new();
        case.local_mutation = Some(("storage", round));
        case.held(Refusal::StorageUnavailable, round - 1);
    }
}

#[test]
fn amended_coordinator_history_drift_before_audit_and_after_audit_stops_without_spawn() {
    for round in [1, 2] {
        let mut case = Case::new();
        case.local_mutation = Some(("history", round));
        case.held(Refusal::AuthorizationFailed, round - 1);
    }
}
