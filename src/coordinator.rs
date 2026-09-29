use crate::{
    claim::{self, AssignmentWriter, ClaimError},
    config::Config,
    eligibility::Candidate,
    github::{
        project::ProjectReader,
        pull_request::{LookupError, LookupResult, PullRequestReader, lookup},
    },
    state::{PausePrEvidence, PausePrStatus, StateError, StateStore},
    supervisor::{self, LaunchPlan, SupervisorError},
    worktree::{self, WorktreeError},
};
use std::{
    collections::HashSet,
    io::Read,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptReview {
    Completed(supervisor::Reconciliation),
    Held(supervisor::Reconciliation),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptReport {
    pub task_id: String,
    pub attempt_id: String,
    pub review: AttemptReview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceHold {
    pub task_id: String,
    pub kind: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StartupReport {
    pub attempts: Vec<AttemptReport>,
    pub source_holds: Vec<SourceHold>,
}

impl StartupReport {
    pub fn scheduling_blocked(&self) -> bool {
        !self.source_holds.is_empty()
            || self
                .attempts
                .iter()
                .any(|attempt| matches!(attempt.review, AttemptReview::Error(_)))
    }
}

/// Reconcile every durable nonterminal attempt, even if a prior attempt cannot
/// be verified. An error never releases its reservation and cannot be ignored by
/// the scheduler. Source operations require proof before new selection.
pub fn startup_reconcile_all<Q: PullRequestReader>(
    store: &mut StateStore,
    prs: &mut Q,
) -> Result<StartupReport, StateError> {
    let mut report = StartupReport::default();
    for (task_id, attempt_id) in store.pending_attempts()? {
        let review = match reconcile_with_pr(store, &task_id, &attempt_id, prs) {
            Ok(result @ supervisor::Reconciliation::Completed { .. }) => {
                AttemptReview::Completed(result)
            }
            Ok(result @ supervisor::Reconciliation::Held { .. }) => AttemptReview::Held(result),
            Err(error) => AttemptReview::Error(error.to_string()),
        };
        report.attempts.push(AttemptReport {
            task_id,
            attempt_id,
            review,
        });
    }
    report.source_holds = store
        .unresolved_sources()?
        .into_iter()
        .map(|(task_id, kind)| SourceHold { task_id, kind })
        .collect();
    Ok(report)
}

/// Process and receipt proof precede the PR read; only an exhaustive absent
/// result may make a stopped task resumable. No lookup starts another worker.
pub fn reconcile_with_pr<Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    prs: &mut Q,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let result = supervisor::reconcile_attempt(store, task_id, attempt_id)?;
    if !matches!(result, supervisor::Reconciliation::Completed { .. })
        || !store.stopped_exit_for_pause(task_id, attempt_id)?
    {
        return Ok(result);
    }
    finish_verified_stopped_attempt_with_pr(store, task_id, attempt_id, prs, result)
}

fn finish_verified_stopped_attempt_with_pr<Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    prs: &mut Q,
    result: supervisor::Reconciliation,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let selection = store
        .selection_evidence(task_id)?
        .ok_or(StateError::InvalidSelection)?;
    let repository = &selection.candidate.mapping.code_repository;
    let status = match lookup(prs, repository, &selection.candidate.issue_url) {
        Ok(LookupResult::Absent) => PausePrStatus::Absent,
        Ok(LookupResult::OpenPreexisting(_)) => PausePrStatus::Open,
        Ok(LookupResult::Ambiguous(_)) => PausePrStatus::Ambiguous,
        Err(error) => PausePrStatus::Error {
            category: error.category,
            code: error.code.to_owned(),
            http_status: error.status,
        },
    };
    let observed_at_unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SupervisorError::IdentityUnavailable)?
        .as_secs();
    let proof = PausePrEvidence {
        observed_at_unix_secs,
        repository: repository.clone(),
        status,
    };
    store.record_pause_pr_lookup(task_id, attempt_id, &proof)?;
    let reason = match proof.status {
        PausePrStatus::Absent => return Ok(result),
        PausePrStatus::Open => "pause PR present",
        PausePrStatus::Ambiguous => "pause PR ambiguous",
        PausePrStatus::Error { .. } => "pause PR read failed",
    };
    Ok(supervisor::Reconciliation::Held {
        reason: reason.into(),
    })
}

pub trait IdCreator {
    fn create(&mut self) -> Result<String, std::io::Error>;
}

pub struct OsIdCreator;

impl IdCreator for OsIdCreator {
    fn create(&mut self) -> Result<String, std::io::Error> {
        let mut bytes = [0u8; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

pub trait SupervisorLauncher {
    fn launch(&mut self, store: &mut StateStore, plan: &LaunchPlan) -> Result<(), SupervisorError>;
}

pub struct ProductionLauncher;

impl SupervisorLauncher for ProductionLauncher {
    fn launch(&mut self, store: &mut StateStore, plan: &LaunchPlan) -> Result<(), SupervisorError> {
        supervisor::execute(store, plan)
    }
}

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Claim(#[from] ClaimError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    PullRequest(#[from] LookupError),
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    #[error("claim changed before launch")]
    ChangedClaim,
    #[error("pull request is no longer absent")]
    ExistingPr,
}

#[derive(Debug, Error)]
pub enum ScheduleError {
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("ID generation failed: {0}")]
    Id(#[from] std::io::Error),
}

pub struct DispatchDependencies<'a, P, Q, W, L> {
    pub config: &'a Config,
    pub config_revision: &'a str,
    pub task_id: &'a str,
    pub attempt_id: &'a str,
    pub projects: &'a mut P,
    pub prs: &'a mut Q,
    pub assignments: &'a mut W,
    pub launcher: &'a mut L,
}

/// Dispatches a single preselected issue. Callers must reconcile outstanding
/// intents before scheduling; this path never resumes or retries a prior task.
pub fn dispatch_one<P, Q, W, L>(
    store: &mut StateStore,
    candidate: &Candidate,
    dependencies: DispatchDependencies<'_, P, Q, W, L>,
) -> Result<LaunchPlan, DispatchError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    W: AssignmentWriter,
    L: SupervisorLauncher,
{
    let DispatchDependencies {
        config,
        config_revision,
        task_id,
        attempt_id,
        projects,
        prs,
        assignments,
        launcher,
    } = dependencies;
    store.ensure_dispatch_capacity()?;
    store.create_task(task_id, candidate, config_revision, config)?;
    let result = (|| {
        worktree::preflight(task_id, &config.worktree_root, &candidate.mapping)?;
        claim::claim(
            store,
            task_id,
            candidate,
            &config.assignment_login,
            projects,
            prs,
            assignments,
        )?;
        worktree::ensure_worktree(store, task_id, &config.worktree_root, &candidate.mapping)?;
        let (_, issue) = claim::fresh(projects, candidate)?;
        if issue.assignees != [config.assignment_login.as_str()] {
            return Err(DispatchError::ChangedClaim);
        }
        if lookup(
            prs,
            &candidate.mapping.code_repository,
            &candidate.issue_url,
        )? != LookupResult::Absent
        {
            return Err(DispatchError::ExistingPr);
        }
        let plan = supervisor::prepare_initial(store, task_id, attempt_id)?;
        launcher.launch(store, &plan)?;
        Ok(plan)
    })();
    if let Err(error) = &result {
        // Keep the reason bounded to a stage/type: external error strings may carry credentials.
        let reason = match error {
            DispatchError::Claim(_) => "claim failed",
            DispatchError::Worktree(_) => "worktree failed",
            DispatchError::ChangedClaim => "prelaunch claim changed",
            DispatchError::ExistingPr => "prelaunch PR present",
            DispatchError::PullRequest(_) => "prelaunch PR read failed",
            DispatchError::Supervisor(_) => "launch preparation or dispatch failed",
            DispatchError::State(_) => "state transition failed",
        };
        store.hold_task(task_id, reason)?;
    }
    result
}

pub struct ScheduleDependencies<'a, P, Q, W, L, I> {
    pub config: &'a Config,
    pub config_revision: &'a str,
    pub projects: &'a mut P,
    pub prs: &'a mut Q,
    pub assignments: &'a mut W,
    pub launcher: &'a mut L,
    pub ids: &'a mut I,
}

#[derive(Debug, Default)]
pub struct ScheduleReport {
    pub startup: StartupReport,
    pub launched: Vec<LaunchPlan>,
    pub skipped_existing: Vec<(String, String)>,
    pub capacity_full: bool,
}

/// Selected candidates are finite; no held or previously selected issue is
/// retried. A failed dispatch stops this pass without trying another worker.
pub fn schedule_candidates<P, Q, W, L, I>(
    store: &mut StateStore,
    candidates: Vec<Candidate>,
    dependencies: ScheduleDependencies<'_, P, Q, W, L, I>,
) -> Result<ScheduleReport, ScheduleError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    W: AssignmentWriter,
    L: SupervisorLauncher,
    I: IdCreator,
{
    let ScheduleDependencies {
        config,
        config_revision,
        projects,
        prs,
        assignments,
        launcher,
        ids,
    } = dependencies;
    let mut report = ScheduleReport {
        startup: startup_reconcile_all(store, prs)?,
        ..Default::default()
    };
    if report.startup.scheduling_blocked() {
        return Ok(report);
    }
    let mut seen = HashSet::new();
    for candidate in candidates {
        let identity = (
            candidate.tracker_repo_id.clone(),
            candidate.issue_node_id.clone(),
        );
        if !seen.insert(identity.clone()) || store.existing_issue(&identity.0, &identity.1)? {
            report.skipped_existing.push(identity);
            continue;
        }
        if !store.unresolved_sources()?.is_empty() {
            report.startup.source_holds = store
                .unresolved_sources()?
                .into_iter()
                .map(|(task_id, kind)| SourceHold { task_id, kind })
                .collect();
            break;
        }
        match store.ensure_dispatch_capacity() {
            Ok(()) => {}
            Err(StateError::Capacity { .. }) => {
                report.capacity_full = true;
                break;
            }
            Err(error) => return Err(error.into()),
        }
        let task_id = format!("task-{}", ids.create()?);
        let attempt_id = format!("attempt-{}", ids.create()?);
        let plan = dispatch_one(
            store,
            &candidate,
            DispatchDependencies {
                config,
                config_revision,
                task_id: &task_id,
                attempt_id: &attempt_id,
                projects,
                prs,
                assignments,
                launcher,
            },
        )?;
        report.launched.push(plan);
    }
    Ok(report)
}

pub struct ResumeDependencies<'a, P, Q, L> {
    pub task_id: &'a str,
    pub attempt_id: &'a str,
    pub projects: &'a mut P,
    pub prs: &'a mut Q,
    pub launcher: &'a mut L,
}

/// Continues a previously reconciled, paused task using only its stored selection.
/// Failed evidence checks hold the task without reserving another attempt.
pub fn resume_one<P, Q, L>(
    store: &mut StateStore,
    dependencies: ResumeDependencies<'_, P, Q, L>,
) -> Result<LaunchPlan, DispatchError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    L: SupervisorLauncher,
{
    let ResumeDependencies {
        task_id,
        attempt_id,
        projects,
        prs,
        launcher,
    } = dependencies;
    // Do not change the phase of an active or unverified task to held.
    store.resume_context(task_id)?;
    let result = (|| {
        let selection = store
            .selection_evidence(task_id)?
            .ok_or(StateError::InvalidSelection)?;
        let candidate = &selection.candidate;
        let (_, issue) = claim::fresh(projects, candidate)?;
        if selection
            .effective_config
            .assignment_login
            .trim()
            .is_empty()
            || issue.assignees != [selection.effective_config.assignment_login.as_str()]
        {
            return Err(DispatchError::ChangedClaim);
        }
        if lookup(
            prs,
            &candidate.mapping.code_repository,
            &candidate.issue_url,
        )? != LookupResult::Absent
        {
            return Err(DispatchError::ExistingPr);
        }
        let plan = supervisor::prepare_resume(store, task_id, attempt_id)?;
        launcher.launch(store, &plan)?;
        Ok(plan)
    })();
    if let Err(error) = &result {
        let reason = match error {
            DispatchError::Claim(_) | DispatchError::ChangedClaim => "resume claim changed",
            DispatchError::ExistingPr => "resume PR present",
            DispatchError::PullRequest(_) => "resume PR read failed",
            DispatchError::Supervisor(_) => "resume preparation or dispatch failed",
            DispatchError::State(_) => "resume state transition failed",
            DispatchError::Worktree(_) => "resume worktree failed",
        };
        store.hold_task(task_id, reason)?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pull_request::ErrorCategory;
    use rusqlite::params;
    use serde_json::{Value, json};

    struct FakePr {
        scenario: &'static str,
        lookups: usize,
    }

    impl PullRequestReader for FakePr {
        fn page(&mut self, repository: &str, page: u32) -> Result<Vec<Value>, LookupError> {
            assert_eq!(repository, "org/code");
            assert_eq!(page, 1);
            self.lookups += 1;
            let entry = |number| {
                json!({
                    "number": number,
                    "body": "Tracker-Issue: https://github.com/org/tracker/issues/1"
                })
            };
            match self.scenario {
                "absent" => Ok(vec![]),
                "open" => Ok(vec![entry(7)]),
                "ambiguous" => Ok(vec![entry(7), entry(8)]),
                "error" => Err(LookupError {
                    category: ErrorCategory::Transport,
                    code: "offline",
                    status: None,
                }),
                _ => unreachable!(),
            }
        }

        fn detail(&mut self, repository: &str, number: u64) -> Result<Value, LookupError> {
            assert_eq!(repository, "org/code");
            Ok(json!({
                "id": number, "state": "open",
                "html_url": format!("https://github.com/org/code/pull/{number}"),
                "base": {"repo": {"full_name": "org/code"}, "ref": "main"},
                "head": {"repo": {"full_name": "org/code"}, "ref": "branch"},
                "user": {"login": "bot"}, "draft": false
            }))
        }
    }

    fn stopped_store(dir: &tempfile::TempDir) -> StateStore {
        let store = StateStore::open(dir.path(), 1).unwrap();
        let connection = rusqlite::Connection::open(dir.path().join("state.sqlite3")).unwrap();
        let selection = json!({
            "candidate": {
                "project_id": "p", "item_id": "i", "repository": "org/tracker",
                "issue_node_id": "issue", "issue_number": 1,
                "issue_url": "https://github.com/org/tracker/issues/1",
                "tracker_repo_id": "repo", "milestone_id": null, "milestone_title": null,
                "observed_at_unix_secs": 1, "observed_state": "open",
                "observed_assignees": [], "observed_labels": [],
                "observed_project_fields": [], "marker": {"kind": "label", "name": "ready"},
                "mapping": {
                    "tracker_repository": "org/tracker", "code_repository": "org/code",
                    "checkout": "checkout", "base_branch": "main", "push_remote": "origin",
                    "allowed_pr_head_repository": "org/code", "allowed_pr_author": "bot"
                },
                "source": {"project_id": "p", "repositories": ["org/tracker"],
                    "ready_marker": {"kind": "label", "name": "ready"}, "milestone": null}
            },
            "config_revision": "r",
            "effective_config": {
                "state_root": "state", "worktree_root": "private", "capacity": 1,
                "assignment_login": "bot", "sources": [], "mappings": [],
                "initial": {"executable": "worker", "args": []},
                "resume": {"executable": "worker", "args": []}
            }
        });
        let receipt = json!({
            "attempt_id": "attempt", "child_pid": 123, "boot_identity": "boot",
            "child_start_identity": "start", "exit_code": null, "signal": 15,
            "stdout_path": "stdout", "stdout_bytes": 0,
            "stderr_path": "stderr", "stderr_bytes": 0, "stop_signals": [15]
        });
        connection.execute(
            "INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision)
             VALUES('task','repo','issue','org/tracker',1,'held','r')", [],
        ).unwrap();
        connection
            .execute(
                "INSERT INTO attempts(id,task_id,lifecycle,outcome)
             VALUES('attempt','task','completed','exit_code=None;signal=Some(15)')",
                [],
            )
            .unwrap();
        connection.execute(
            "INSERT INTO reservations(attempt_id,task_id,status) VALUES('attempt','task','released')", [],
        ).unwrap();
        connection
            .execute(
                "INSERT INTO intents(id,task_id,attempt_id,kind,detail)
             VALUES('stop','task','attempt','stop','')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO intents(id,task_id,attempt_id,kind,detail)
             VALUES('launch','task','attempt','launch','plan')",
                [],
            )
            .unwrap();
        for kind in ["claim_verified", "worktree_created"] {
            connection
                .execute(
                    "INSERT INTO evidence(task_id,kind,payload) VALUES('task',?1,'proof')",
                    [kind],
                )
                .unwrap();
        }
        connection.execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task',NULL,'selection',?1)",
            [selection.to_string()],
        ).unwrap();
        connection.execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','attempt','attempt_exit',?1)",
            [receipt.to_string()],
        ).unwrap();
        store
    }

    #[test]
    fn verified_stopped_attempt_pr_outcomes_gate_slot() {
        for (scenario, expected) in [
            ("absent", PausePrStatus::Absent),
            ("open", PausePrStatus::Open),
            ("ambiguous", PausePrStatus::Ambiguous),
            (
                "error",
                PausePrStatus::Error {
                    category: ErrorCategory::Transport,
                    code: "offline".into(),
                    http_status: None,
                },
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut store = stopped_store(&dir);
            let mut prs = FakePr {
                scenario,
                lookups: 0,
            };
            let result = finish_verified_stopped_attempt_with_pr(
                &mut store,
                "task",
                "attempt",
                &mut prs,
                supervisor::Reconciliation::Completed {
                    exit_code: None,
                    signal: Some(15),
                },
            )
            .unwrap();
            assert_eq!(prs.lookups, 1, "{scenario}");
            let proof: PausePrEvidence = serde_json::from_str(
                &store
                    .evidence_payload("task", Some("attempt"), "pause_pr_lookup")
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(proof.repository, "org/code", "{scenario}");
            assert!(proof.observed_at_unix_secs > 0);
            assert_eq!(proof.status, expected, "{scenario}");
            if scenario == "absent" {
                assert!(matches!(
                    result,
                    supervisor::Reconciliation::Completed { .. }
                ));
                assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("paused"));
                assert!(store.ensure_dispatch_capacity().is_ok());
            } else {
                assert!(matches!(result, supervisor::Reconciliation::Held { .. }));
                assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
                assert!(matches!(
                    store.ensure_dispatch_capacity(),
                    Err(StateError::Capacity { .. })
                ));
            }
            assert_eq!(store.reservation_count().unwrap(), 0, "{scenario}");
            let connection = rusqlite::Connection::open(dir.path().join("state.sqlite3")).unwrap();
            let count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM evidence WHERE task_id='task' AND attempt_id='attempt' AND kind='pause_pr_lookup'",
                params![], |row| row.get(0),
            ).unwrap();
            assert_eq!(count, 1, "{scenario}");
        }
    }
}
