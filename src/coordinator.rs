use crate::{
    claim::{self, AssignmentWriter, ClaimError},
    config::Config,
    eligibility::Candidate,
    github::{
        project::{ProjectReader, enumerate_target},
        pull_request::{LookupError, LookupResult, PullRequestReader, lookup},
    },
    pr_evidence::{VerifiedOpenPr, expected_for_task},
    state::{ExitPrEvidence, PausePrEvidence, PausePrStatus, StateError, StateStore},
    supervisor::{self, LaunchPlan, SupervisorError},
    worktree::{self, WorktreeError, WorktreeInspection},
};
use serde::Serialize;
use std::{
    collections::HashSet,
    io::Read,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptReview {
    Running,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceReconciliation {
    pub task_id: String,
    pub status: &'static str,
    pub reasons: Vec<&'static str>,
    pub issue_state: Option<String>,
    pub assignees: Option<Vec<String>>,
    pub marker_present: Option<bool>,
    pub project_membership: Option<bool>,
    pub worktree: Option<WorktreeInspection>,
}

/// Observe unfinished source operations without verifying an intent, assigning,
/// adopting a worktree, or releasing the task for scheduling.
pub fn reconcile_source<P: ProjectReader>(
    store: &mut StateStore,
    task_id: &str,
    projects: &mut P,
) -> Result<SourceReconciliation, StateError> {
    if store.task_phase(task_id)?.is_none() || store.latest_attempt(task_id)?.is_some() {
        return Err(StateError::InvalidSelection);
    }
    let mut report = SourceReconciliation {
        task_id: task_id.to_owned(),
        status: "held",
        reasons: vec![],
        issue_state: None,
        assignees: None,
        marker_present: None,
        project_membership: None,
        worktree: None,
    };
    if let Some(selection) = store.selection_evidence(task_id)? {
        let candidate = &selection.candidate;
        match enumerate_target(
            projects,
            &candidate.project_id,
            &candidate.repository,
            candidate.issue_number,
        ) {
            Ok(items) => {
                let matching = items
                    .iter()
                    .filter(|(item, _)| {
                        item.item_id == candidate.item_id
                            && item.issue_node_id == candidate.issue_node_id
                            && item.tracker_repo_id == candidate.tracker_repo_id
                    })
                    .collect::<Vec<_>>();
                report.project_membership = Some(matching.len() == 1 && items.len() == 1);
                if let [(item, issue)] = matching.as_slice() {
                    report.issue_state = Some(issue.state.clone());
                    report.assignees = Some(issue.assignees.clone());
                    let marker = match &candidate.marker {
                        crate::config::Marker::Label { name } => issue.labels.contains(name),
                        crate::config::Marker::ProjectField { name, value } => {
                            !item.unsupported_fields.contains(name)
                                && item.fields.contains(&(name.clone(), value.clone()))
                        }
                    };
                    report.marker_present = Some(marker);
                    if issue.node_id != candidate.issue_node_id
                        || issue.repository != candidate.repository
                        || issue.tracker_repo_id != candidate.tracker_repo_id
                        || issue.url != candidate.issue_url
                        || issue.milestone != candidate.milestone_title
                        || issue.milestone_id != candidate.milestone_id
                        || candidate
                            .source
                            .milestone
                            .as_ref()
                            .is_some_and(|title| issue.milestone.as_ref() != Some(title))
                    {
                        report.reasons.push("issue_identity_mismatch");
                    }
                    if issue.state != "open" {
                        report.reasons.push("issue_not_open");
                    }
                    if !marker {
                        report.reasons.push("marker_missing");
                    }
                    if issue.assignees != [selection.effective_config.assignment_login.as_str()]
                        && !issue.assignees.is_empty()
                    {
                        report.reasons.push("assignees_unexpected");
                    }
                } else {
                    report.reasons.push("project_membership_mismatch");
                }
                if report.project_membership == Some(false)
                    && !report.reasons.contains(&"project_membership_mismatch")
                {
                    report.reasons.push("project_membership_mismatch");
                }
            }
            Err(_) => report.reasons.push("source_read_failed"),
        }
        if let Some(detail) = store.source_claim_intent(task_id)? {
            let expected = serde_json::json!({"principal": selection.effective_config.assignment_login,
                "repository": candidate.repository, "number": candidate.issue_number});
            if serde_json::from_str::<serde_json::Value>(&detail).ok() != Some(expected) {
                report.reasons.push("claim_intent_mismatch");
            }
            if report.assignees.as_deref() == Some(&[][..]) {
                report.reasons.push("assignment_not_observed");
            }
            report.reasons.push("claim_intent_unverified");
        }
        if let Some(record) = store.worktree_record(task_id)? {
            let root = &selection.effective_config.worktree_root;
            let expected_path = std::fs::canonicalize(root)
                .unwrap_or_else(|_| root.clone())
                .join(task_id);
            if record.intent.path != expected_path {
                report.reasons.push("worktree_intent_mismatch");
            } else {
                match worktree::inspect_record(&record, &candidate.mapping) {
                    Ok(inspection) => {
                        if inspection != WorktreeInspection::IdentityMatches {
                            report.reasons.push("worktree_unverified");
                        }
                        report.worktree = Some(inspection);
                    }
                    Err(_) => report.reasons.push("worktree_read_failed"),
                }
            }
        }
    } else {
        report.reasons.push("selection_missing");
    }
    if report.reasons.is_empty() {
        report.reasons.push("prelaunch_not_verified");
    }
    store.record_evidence(
        task_id,
        None,
        "source_observation",
        &serde_json::to_string(&report)?,
    )?;
    Ok(report)
}

fn completion_claim_failure_reason(error: &ClaimError) -> &'static str {
    match error {
        ClaimError::Changed => "completion claim changed",
        ClaimError::Source(_) => "completion claim read failed",
        _ => unreachable!("completion claim verification only returns source or changed errors"),
    }
}

/// Verify that the selected issue is still assigned solely to the configured
/// login before completion is accepted.
pub(crate) fn verify_completion_claim<P: ProjectReader>(
    projects: &mut P,
    selection: &crate::state::SelectionEvidence,
) -> Result<(), ClaimError> {
    let login = &selection.effective_config.assignment_login;
    if login.trim().is_empty() {
        return Err(ClaimError::Changed);
    }
    let (_, issue) = claim::fresh(projects, &selection.candidate)?;
    if issue.assignees != [login.as_str()] {
        return Err(ClaimError::Changed);
    }
    Ok(())
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
pub fn startup_reconcile_all<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    projects: &mut P,
    prs: &mut Q,
) -> Result<StartupReport, StateError> {
    let mut report = StartupReport::default();
    for (task_id, attempt_id) in store.pending_attempts()? {
        let review = match reconcile_with_pr(store, &task_id, &attempt_id, projects, prs) {
            Ok(supervisor::Reconciliation::Running) => AttemptReview::Running,
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

/// Process and receipt proof precede the PR read. Only exhaustive absence
/// releases a stopped task for resume or a natural exit for attention.
pub fn reconcile_with_pr<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    projects: &mut P,
    prs: &mut Q,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let result = supervisor::reconcile_attempt(store, task_id, attempt_id)?;
    if !matches!(result, supervisor::Reconciliation::Completed { .. }) {
        return Ok(result);
    }
    if store.stopped_exit_for_pause(task_id, attempt_id)? {
        return finish_verified_stopped_attempt_with_pr(
            store, task_id, attempt_id, projects, prs, result,
        );
    }
    if store.natural_exit_for_attention(task_id, attempt_id)? {
        return finish_verified_natural_exit_with_pr(
            store, task_id, attempt_id, projects, prs, result,
        );
    }
    Ok(result)
}

fn finish_verified_stopped_attempt_with_pr<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    projects: &mut P,
    prs: &mut Q,
    result: supervisor::Reconciliation,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let selection = store
        .selection_evidence(task_id)?
        .ok_or(StateError::InvalidSelection)?;
    let repository = &selection.candidate.mapping.code_repository;
    let lookup_result = lookup(prs, repository, &selection.candidate.issue_url);
    let status = lookup_status(&lookup_result);
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
    match lookup_result {
        Ok(LookupResult::Absent) => Ok(result),
        Ok(LookupResult::OpenPreexisting(pr)) => verify_and_record_open_pr(
            store,
            task_id,
            attempt_id,
            projects,
            prs,
            (*pr, observed_at_unix_secs),
            result,
        ),
        Ok(LookupResult::Ambiguous(_)) => Ok(supervisor::Reconciliation::Held {
            reason: "pause PR ambiguous".into(),
        }),
        Err(_) => Ok(supervisor::Reconciliation::Held {
            reason: "pause PR read failed".into(),
        }),
    }
}

fn finish_verified_natural_exit_with_pr<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    projects: &mut P,
    prs: &mut Q,
    result: supervisor::Reconciliation,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let selection = store
        .selection_evidence(task_id)?
        .ok_or(StateError::InvalidSelection)?;
    let repository = &selection.candidate.mapping.code_repository;
    let lookup_result = lookup(prs, repository, &selection.candidate.issue_url);
    let observed_at_unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SupervisorError::IdentityUnavailable)?
        .as_secs();
    let lookup_status = match &lookup_result {
        Ok(LookupResult::Absent) => PausePrStatus::Absent,
        Ok(LookupResult::OpenPreexisting(_)) => PausePrStatus::Open,
        Ok(LookupResult::Ambiguous(_)) => PausePrStatus::Ambiguous,
        Err(error) => PausePrStatus::Error {
            category: error.category,
            code: error.code.to_owned(),
            http_status: error.status,
        },
    };
    let proof = ExitPrEvidence {
        observed_at_unix_secs,
        repository: repository.clone(),
        status: lookup_status,
    };
    store.record_exit_pr_lookup(task_id, attempt_id, &proof)?;
    match lookup_result {
        Ok(LookupResult::Absent) => Ok(result),
        Ok(LookupResult::OpenPreexisting(pr)) => {
            let verified = (|| {
                let login = prs
                    .authenticated_identity()
                    .map_err(|_| "exit PR identity unavailable")?;
                let expected = expected_for_task(store, task_id, prs, &login)
                    .map_err(|_| "exit PR evidence unavailable")?;
                VerifiedOpenPr::from_matching(
                    *pr,
                    &expected,
                    &login,
                    attempt_id,
                    observed_at_unix_secs,
                )
                .map_err(|_| "exit PR verification failed")
            })();
            match verified {
                Ok(verified) => {
                    if let Err(error) = verify_completion_claim(projects, &selection) {
                        let reason = completion_claim_failure_reason(&error);
                        store.record_evidence(task_id, Some(attempt_id), "held_reason", reason)?;
                        return Ok(supervisor::Reconciliation::Held {
                            reason: reason.into(),
                        });
                    }
                    store.record_verified_open_pr(task_id, attempt_id, &verified)?;
                    Ok(result)
                }
                Err(reason) => {
                    store.record_evidence(task_id, Some(attempt_id), "held_reason", reason)?;
                    Ok(supervisor::Reconciliation::Held {
                        reason: reason.into(),
                    })
                }
            }
        }
        Ok(LookupResult::Ambiguous(_)) => Ok(supervisor::Reconciliation::Held {
            reason: "exit PR ambiguous".into(),
        }),
        Err(_) => Ok(supervisor::Reconciliation::Held {
            reason: "exit PR read failed".into(),
        }),
    }
}

fn lookup_status(result: &Result<LookupResult, LookupError>) -> PausePrStatus {
    match result {
        Ok(LookupResult::Absent) => PausePrStatus::Absent,
        Ok(LookupResult::OpenPreexisting(_)) => PausePrStatus::Open,
        Ok(LookupResult::Ambiguous(_)) => PausePrStatus::Ambiguous,
        Err(error) => PausePrStatus::Error {
            category: error.category,
            code: error.code.to_owned(),
            http_status: error.status,
        },
    }
}

fn verify_and_record_open_pr<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    projects: &mut P,
    prs: &mut Q,
    evidence: (crate::github::pull_request::PullRequestEvidence, u64),
    result: supervisor::Reconciliation,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let (pr, observed_at_unix_secs) = evidence;
    let verified = (|| {
        let login = prs
            .authenticated_identity()
            .map_err(|_| "exit PR identity unavailable")?;
        let expected = expected_for_task(store, task_id, prs, &login)
            .map_err(|_| "exit PR evidence unavailable")?;
        VerifiedOpenPr::from_matching(pr, &expected, &login, attempt_id, observed_at_unix_secs)
            .map_err(|_| "exit PR verification failed")
    })();
    match verified {
        Ok(verified) => {
            let selection = store
                .selection_evidence(task_id)?
                .ok_or(StateError::InvalidSelection)?;
            if let Err(error) = verify_completion_claim(projects, &selection) {
                let reason = completion_claim_failure_reason(&error);
                store.record_evidence(task_id, Some(attempt_id), "held_reason", reason)?;
                return Ok(supervisor::Reconciliation::Held {
                    reason: reason.into(),
                });
            }
            store.record_verified_open_pr(task_id, attempt_id, &verified)?;
            Ok(result)
        }
        Err(reason) => {
            store.record_evidence(task_id, Some(attempt_id), "held_reason", reason)?;
            Ok(supervisor::Reconciliation::Held {
                reason: reason.to_owned(),
            })
        }
    }
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
        startup: startup_reconcile_all(store, projects, prs)?,
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
    use crate::github::project::{Issue, Page, ProjectItem, ProjectReadError};
    use crate::github::pull_request::ErrorCategory;
    use rusqlite::params;
    use serde_json::{Value, json};

    struct FakePr {
        scenario: &'static str,
        lookups: usize,
    }

    impl PullRequestReader for FakePr {
        fn authenticated_identity(&mut self) -> Result<String, LookupError> {
            Ok("bot".into())
        }

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

        fn repository_identity(&mut self, name: &str) -> Result<u64, LookupError> {
            match name {
                "org/code" => Ok(10),
                "org/head" => Ok(20),
                _ => Err(LookupError {
                    category: ErrorCategory::Malformed,
                    code: "unexpected-repository",
                    status: None,
                }),
            }
        }

        fn detail(&mut self, repository: &str, number: u64) -> Result<Value, LookupError> {
            assert_eq!(repository, "org/code");
            Ok(json!({
                "id": number, "number": number, "state": "open",
                "html_url": format!("https://github.com/org/code/pull/{number}"),
                "body": "Tracker-Issue: https://github.com/org/tracker/issues/1",
                "created_at": "2026-01-01T00:00:00Z",
                "base": {"repo": {"id": 10, "full_name": "org/code"}, "ref": "main"},
                "head": {"repo": {"id": 10, "full_name": "org/code"}, "ref": "branch", "sha": "abc123"},
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
        struct NoProject;

        impl ProjectReader for NoProject {
            fn page(
                &mut self,
                _: &str,
                _: Option<&str>,
            ) -> Result<Page<ProjectItem>, ProjectReadError> {
                panic!("project page should not be read")
            }

            fn issue(&mut self, _: &ProjectItem) -> Result<Issue, ProjectReadError> {
                panic!("issue should not be read")
            }
        }

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
            let mut projects = NoProject;
            let result = finish_verified_stopped_attempt_with_pr(
                &mut store,
                "task",
                "attempt",
                &mut projects,
                &mut prs,
                supervisor::Reconciliation::Completed {
                    exit_code: Some(17),
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
            } else if scenario == "open" {
                assert_eq!(
                    result,
                    supervisor::Reconciliation::Held {
                        reason: "exit PR evidence unavailable".into(),
                    }
                );
                assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
                assert!(
                    store
                        .evidence_payload("task", Some("attempt"), "verified_open_pr")
                        .unwrap()
                        .is_none()
                );
                assert!(matches!(
                    store.ensure_dispatch_capacity(),
                    Err(StateError::Capacity { .. })
                ));
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
