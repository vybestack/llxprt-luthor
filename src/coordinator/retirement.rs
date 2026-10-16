//! Retire tasks whose work is over so they stop holding a capacity slot.
//!
//! A task is over when its tracker issue is closed or when its saved verified
//! PR is merged or closed. Both are read fresh: the issue directly and the PR
//! by its immutable identity, never through the open-PR search, which lags a
//! merge. A task with any attempt that has no verified exit is running and is
//! never retired here.
use super::notes::TaskNote;
use crate::{
    cleanup::{CleanupError, terminal_pr},
    eligibility::Candidate,
    github::{
        project::{Issue, ProjectItem, ProjectReader},
        pull_request::PullRequestReader,
    },
    state::{
        StateError, StateStore, pr_repair,
        retirement::{self, RetiredPhase, Retirement},
        task_records,
    },
};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetiredTask {
    pub task_id: String,
    pub phase: RetiredPhase,
    pub reason: String,
}

/// What one sweep did. Tasks whose state could not be read from GitHub, or that
/// cannot be retired yet, stay as they were and are listed with the reason.
#[derive(Debug, Default, Serialize)]
pub struct RetirementReport {
    pub retired: Vec<RetiredTask>,
    pub retained: Vec<TaskNote>,
}

pub(crate) enum Observation {
    /// The work is over; the retirement to record.
    Finished(Box<Retirement>),
    Unfinished,
    /// Nothing may be concluded; the reason names what could not be read.
    Unavailable(String),
}

pub(crate) enum Settlement {
    Retired(RetiredTask),
    Unfinished,
    Unavailable(String),
    /// Finished, but the task cannot be retired now (for example it has a
    /// worker that has not been reconciled).
    Blocked(&'static str),
}

enum PrView {
    NoneSaved,
    Open,
    Terminal(Value),
    Unavailable(String),
}

enum IssueView {
    Open,
    Closed(Value),
    Unavailable(String),
}

/// Visits every non-running attempt-free-of-doubt task once and retires the
/// finished ones. Only a failure to read or write local state aborts it.
pub fn retire_finished_tasks<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    projects: &mut P,
    prs: &mut Q,
) -> Result<RetirementReport, StateError> {
    let mut report = RetirementReport::default();
    for task in retirement::candidates(store)? {
        match retire_if_finished(store, &task, projects, prs)? {
            Settlement::Retired(retired) => report.retired.push(retired),
            Settlement::Unfinished => {}
            Settlement::Unavailable(reason) => report.retained.push(TaskNote {
                task_id: task,
                reason: format!("finished state not established ({reason})"),
            }),
            Settlement::Blocked(reason) => report.retained.push(TaskNote {
                task_id: task,
                reason: reason.into(),
            }),
        }
    }
    Ok(report)
}

pub(crate) fn retire_if_finished<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task: &str,
    projects: &mut P,
    prs: &mut Q,
) -> Result<Settlement, StateError> {
    match observe_finished(store, task, projects, prs)? {
        Observation::Finished(retirement)
            if crate::github::budget::owns_terminal_settlement()
                && matches!(
                    retirement.phase,
                    RetiredPhase::PrMerged | RetiredPhase::PrClosed
                ) =>
        {
            Ok(Settlement::Blocked(
                "terminal PR delivery awaits configured tracker followup",
            ))
        }
        Observation::Finished(retirement) => settle(store, task, &retirement),
        Observation::Unfinished => Ok(Settlement::Unfinished),
        Observation::Unavailable(reason) => Ok(Settlement::Unavailable(reason)),
    }
}

pub(crate) fn settle(
    store: &mut StateStore,
    task: &str,
    finished: &Retirement,
) -> Result<Settlement, StateError> {
    match retirement::retire(store, task, finished) {
        Ok(()) => Ok(Settlement::Retired(RetiredTask {
            task_id: task.to_owned(),
            phase: finished.phase,
            reason: finished.reason.clone(),
        })),
        Err(StateError::NotRetirable(reason)) => Ok(Settlement::Blocked(reason)),
        Err(error) => Err(error),
    }
}

/// Operator decision to stop a task whose issue is still open. GitHub is not
/// touched. A task that may still be running is refused.
pub fn abandon_task(
    store: &mut StateStore,
    task: &str,
    actor: &str,
    reason: &str,
) -> Result<RetiredTask, StateError> {
    let abandonment = Retirement {
        phase: RetiredPhase::Abandoned,
        reason: reason.to_owned(),
        actor: Some(actor.to_owned()),
        observed_at_unix_secs: now()?,
        evidence: json!({"github_changes": "none"}),
    };
    retirement::retire(store, task, &abandonment)?;
    Ok(RetiredTask {
        task_id: task.to_owned(),
        phase: abandonment.phase,
        reason: abandonment.reason,
    })
}

pub(crate) fn observe_finished<P: ProjectReader, Q: PullRequestReader>(
    store: &StateStore,
    task: &str,
    projects: &mut P,
    prs: &mut Q,
) -> Result<Observation, StateError> {
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    let pr = observe_saved_pr(store, task, prs)?;
    let issue = observe_issue(projects, &selection.candidate);
    let observed_at_unix_secs = now()?;
    Ok(decide(pr, issue, observed_at_unix_secs))
}

fn decide(pr: PrView, issue: IssueView, observed_at_unix_secs: u64) -> Observation {
    let finished = |phase, reason: String, evidence| {
        Observation::Finished(Box::new(Retirement {
            phase,
            reason,
            actor: None,
            observed_at_unix_secs,
            evidence,
        }))
    };
    match (pr, issue) {
        (PrView::Terminal(pr), issue) => {
            let number = pr["number"].clone();
            let (phase, reason) = if pr["state"] == "merged" {
                (
                    RetiredPhase::PrMerged,
                    format!("saved verified PR {number} merged"),
                )
            } else {
                (
                    RetiredPhase::PrClosed,
                    format!("saved verified PR {number} was closed without merging"),
                )
            };
            finished(phase, reason, json!({"pr": pr, "issue": issue.describe()}))
        }
        (pr, IssueView::Closed(issue)) => finished(
            RetiredPhase::IssueClosed,
            format!(
                "tracker issue was closed{}",
                issue["closed_at"]
                    .as_str()
                    .map_or(String::new(), |at| format!(" at {at}"))
            ),
            json!({"issue": issue, "pr": pr.describe()}),
        ),
        (PrView::Unavailable(reason), _) | (_, IssueView::Unavailable(reason)) => {
            Observation::Unavailable(reason)
        }
        (PrView::NoneSaved | PrView::Open, IssueView::Open) => Observation::Unfinished,
    }
}

impl PrView {
    fn describe(&self) -> Value {
        match self {
            Self::NoneSaved => json!({"state": "none_saved"}),
            Self::Open => json!({"state": "open"}),
            Self::Terminal(pr) => pr.clone(),
            Self::Unavailable(reason) => json!({"state": "unavailable", "reason": reason}),
        }
    }
}

impl IssueView {
    fn describe(&self) -> Value {
        match self {
            Self::Open => json!({"state": "open"}),
            Self::Closed(issue) => issue.clone(),
            Self::Unavailable(reason) => json!({"state": "unavailable", "reason": reason}),
        }
    }
}

/// The saved PR is read by id and number from the repository; the search is
/// not consulted.
fn observe_saved_pr<Q: PullRequestReader>(
    store: &StateStore,
    task: &str,
    prs: &mut Q,
) -> Result<PrView, StateError> {
    let saved = match pr_repair::saved_pr(&store.connection, task) {
        Ok(saved) => saved,
        Err(StateError::Sql(rusqlite::Error::QueryReturnedNoRows)) => {
            return Ok(PrView::NoneSaved);
        }
        Err(StateError::Serialization(_) | StateError::LaunchBlocked) => {
            return Ok(PrView::Unavailable("saved verified PR is malformed".into()));
        }
        Err(error) => return Err(error),
    };
    match terminal_pr::observe_by_id(prs, &saved) {
        Ok(Some(pr)) => Ok(PrView::Terminal(pr)),
        Ok(None) => Ok(PrView::Open),
        Err(CleanupError::State(error)) => Err(error),
        Err(error @ (CleanupError::Conflict(_) | CleanupError::PullRequest(_))) => {
            Ok(PrView::Unavailable(error.to_string()))
        }
        Err(_) => unreachable!("terminal PR observation only reads GitHub and saved state"),
    }
}

/// The issue is read directly by the identity recorded at selection. The
/// project listing is not consulted, so removal from the project cannot hide
/// a closed issue.
fn observe_issue<P: ProjectReader>(projects: &mut P, candidate: &Candidate) -> IssueView {
    let item = ProjectItem {
        item_id: candidate.item_id.clone(),
        issue_node_id: candidate.issue_node_id.clone(),
        repository: candidate.repository.clone(),
        tracker_repo_id: candidate.tracker_repo_id.clone(),
        issue_number: candidate.issue_number,
        summary: None,
        fields: vec![],
        unsupported_fields: vec![],
    };
    let issue = match projects.issue(&item) {
        Ok(issue) => issue,
        Err(error) => return IssueView::Unavailable(error.to_string()),
    };
    if !same_issue(&issue, candidate) {
        return IssueView::Unavailable("issue identity changed".into());
    }
    if issue.state != "closed" {
        return IssueView::Open;
    }
    IssueView::Closed(json!({
        "state": "closed", "number": issue.number, "url": issue.url,
        "closed_at": issue.closed_at, "observed_at_unix_secs": issue.observed_at_unix_secs
    }))
}

fn same_issue(issue: &Issue, candidate: &Candidate) -> bool {
    issue.node_id == candidate.issue_node_id
        && issue.repository == candidate.repository
        && issue.tracker_repo_id == candidate.tracker_repo_id
        && issue.number == candidate.issue_number
        && issue.url == candidate.issue_url
}

fn now() -> Result<u64, StateError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| std::io::Error::other("system clock precedes Unix epoch"))?
        .as_secs())
}
