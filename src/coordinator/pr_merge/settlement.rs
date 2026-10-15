//! Upgrade-time settlement is independent of today's worker mapping or cleanup policy.
use super::{
    external,
    types::{MergeError, MergeReport, Outcome, isolate, now},
};
use crate::coordinator::{
    notes::TaskNote,
    retirement::{Observation, RetirementReport, Settlement, observe_finished, settle},
};
use crate::github::project::ProjectReader;
use crate::{
    config::Config,
    github::pull_request::{PrMerger, merge_validation::validate_identity},
    model::VerifiedOpenPr,
    pr_merge::TrackerTarget,
    state::{
        StateError, StateStore, journal, pr_merge, pr_poll, pr_repair,
        retirement::{self, RetiredPhase, Retirement},
        task_records,
    },
};
use rusqlite::OptionalExtension;
use serde_json::json;

pub(crate) fn settle_completed_prs<Q: PrMerger>(
    store: &mut StateStore,
    config: &Config,
    prs: &mut Q,
) -> Result<MergeReport, MergeError> {
    let mut report = MergeReport::default();
    for task in crate::state::pr_poll::completed_candidates(store)? {
        // Luthor's interrupted writes belong to the mandatory merge followup.
        if pr_merge::authorization(store, &task)?.is_some() {
            if crate::state::merge_followup::closure(store, &task)?.is_some() {
                report
                    .cleanup
                    .append(&mut retire_own(store, config, &task, prs)?);
                report.merged.push(task);
            }
            continue;
        }
        let saved = crate::state::cleanup_records::verified_pr(store, &task)?;
        if !pr_poll::terminal_close_pending(store, &task)?
            && !pr_poll::begin(store, &task, "terminal", &saved.head_commit_sha)?
        {
            continue;
        }
        let result = inspect(store, config, &task, &saved, prs);
        if result.is_err() {
            crate::github::budget::defer_task(&task);
        }
        match isolate(store, &task, result)? {
            Outcome::Delivered(proof, _) => {
                report
                    .cleanup
                    .append(&mut publish(store, config, &task, &proof, prs)?);
                report.delivered.push(crate::coordinator::notes::TaskNote {
                    task_id: task,
                    reason: proof.reason,
                });
            }
            Outcome::Waiting(reason) if !reason.is_empty() => {
                report.waiting.push(crate::coordinator::notes::TaskNote {
                    task_id: task,
                    reason,
                })
            }
            _ => {}
        }
    }
    Ok(report)
}

fn inspect<Q: PrMerger>(
    store: &mut StateStore,
    config: &Config,
    task: &str,
    saved: &VerifiedOpenPr,
    prs: &mut Q,
) -> Result<Outcome, MergeError> {
    super::types::verify_account(prs, saved)?;
    let detail = prs.terminal_detail(&saved.repository, saved.number)?;
    if detail["state"] == "open" && detail["merged"] == false {
        return Ok(Outcome::Waiting(String::new()));
    }
    crate::github::budget::defer_task(task);
    finish(
        store,
        task,
        saved,
        &detail,
        mapping_closes(store, config, task)?,
        prs,
    )
}

pub(crate) fn finish<Q: PrMerger>(
    store: &mut StateStore,
    task: &str,
    saved: &VerifiedOpenPr,
    detail: &serde_json::Value,
    close_tracker: bool,
    prs: &mut Q,
) -> Result<Outcome, MergeError> {
    require_retirable(store, task)?;
    let proof = if detail["merged"] == true {
        external::proof(store, task, saved, detail)?
    } else {
        validate_identity(detail, saved)?;
        if detail["state"] != "closed"
            || detail["merged"] != false
            || !detail["closed_at"]
                .as_str()
                .is_some_and(|at| !at.is_empty())
            || !detail["merged_at"].is_null()
        {
            return Err(MergeError::Refused(
                "closed PR terminal state is incomplete",
            ));
        }
        Retirement {
            phase: RetiredPhase::PrClosed,
            reason: format!(
                "saved verified PR {} was closed without merging",
                saved.number
            ),
            actor: None,
            observed_at_unix_secs: now()?,
            evidence: json!({"pr":{
                "id":saved.id,"number":saved.number,"url":saved.url,"repository":saved.repository,
                "state":"closed","head_commit_sha":saved.head_commit_sha,"closed_at":detail["closed_at"]
            }}),
        }
    };
    if close_tracker {
        close_tracker_for_proof(store, task, &proof, prs)?;
    }
    Ok(Outcome::Delivered(Box::new(proof), Default::default()))
}

/// Only a merged delivery and an enabled current tracker mapping authorize a
/// tracker close. A changed worker prompt must not strand historical deliveries.
fn close_delivered_tracker<Q: PrMerger>(
    store: &mut StateStore,
    config: &Config,
    task: &str,
    proof: &Retirement,
    prs: &mut Q,
) -> Result<(), MergeError> {
    if proof.phase == RetiredPhase::PrMerged && mapping_closes(store, config, task)? {
        close_tracker_for_proof(store, task, proof, prs)?;
    }
    Ok(())
}

fn mapping_closes(store: &StateStore, config: &Config, task: &str) -> Result<bool, StateError> {
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    Ok(config
        .mappings
        .iter()
        .any(|m| m.tracker_repository == selection.candidate.repository && !m.merge.is_disabled()))
}

fn close_tracker_for_proof<Q: PrMerger>(
    store: &mut StateStore,
    task: &str,
    proof: &Retirement,
    prs: &mut Q,
) -> Result<(), MergeError> {
    if proof.phase != RetiredPhase::PrMerged {
        return Ok(());
    }
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    let saved = pr_repair::saved_pr(&store.connection, task)?;
    let c = selection.candidate;
    let target = TrackerTarget {
        repository: c.repository,
        number: c.issue_number,
        node_id: c.issue_node_id,
        url: c.issue_url,
    };
    let pending: Option<String> = store
        .connection
        .query_row(
            "SELECT detail FROM intents WHERE task_id=?1 AND kind='terminal_tracker_close'",
            [task],
            |r| r.get(0),
        )
        .optional()
        .map_err(StateError::from)?;
    if let Some(payload) = &pending {
        let prior: serde_json::Value = serde_json::from_str(payload).map_err(StateError::from)?;
        if prior["target"] != json!(target)
            || prior["delivery"]["evidence"]["pr"] != proof.evidence["pr"]
        {
            return Err(MergeError::Refused("pending tracker close proof changed"));
        }
    }
    let mut issue = prs.tracker_issue(&target)?;
    if issue.target != target {
        return Err(MergeError::Refused("tracker read identity changed"));
    }
    if issue.closed_at.is_none() {
        if pending.is_some() {
            return Err(MergeError::Refused(
                "tracker close unresolved; no repeated write authorized",
            ));
        }
        journal::record_intent(
            store,
            &format!("terminal-close-{task}"),
            task,
            None,
            "terminal_tracker_close",
            &json!({"target":target,"delivery":proof}).to_string(),
        )?;
        prs.close_finished_tracker(&saved, &proof.evidence["pr"], &target)?;
        issue = prs.tracker_issue(&target)?;
        if issue.target != target || issue.closed_at.is_none() {
            return Err(MergeError::Refused("tracker close not confirmed"));
        }
    }
    journal::record_evidence(
        store,
        task,
        None,
        "terminal_tracker_closed",
        &json!({"issue":issue,"delivery":proof}).to_string(),
    )?;
    Ok(())
}

/// Daemon settlement additionally honors configured tracker closure for held
/// repairs. A failed/uncertain close leaves the task nonterminal for followup.
pub(crate) fn retire_with_tracker<P: ProjectReader, Q: crate::github::pull_request::PrMerger>(
    store: &mut StateStore,
    config: &crate::config::Config,
    projects: &mut P,
    prs: &mut Q,
) -> Result<RetirementReport, StateError> {
    let mut report = RetirementReport::default();
    for task in retirement::candidates(store)? {
        if !crate::state::pr_poll::terminal_close_pending(store, &task)?
            && !crate::state::pr_poll::begin(store, &task, "retire", "held")?
        {
            continue;
        }
        let observed = observe_finished(store, &task, projects, prs)?;
        let result = match observed {
            Observation::Finished(proof) => match settle_held(store, config, &task, *proof, prs) {
                Ok(result) => result,
                Err(MergeError::State(error)) => return Err(error),
                Err(error) => Settlement::Unavailable(error.to_string()),
            },
            Observation::Unfinished => Settlement::Unfinished,
            Observation::Unavailable(reason) => Settlement::Unavailable(reason),
        };
        match result {
            Settlement::Retired(retired) => report.retired.push(retired),
            Settlement::Unavailable(reason) => report.retained.push(TaskNote {
                task_id: task,
                reason,
            }),
            Settlement::Blocked(reason) => report.retained.push(TaskNote {
                task_id: task,
                reason: reason.into(),
            }),
            Settlement::Unfinished => {}
        }
    }
    Ok(report)
}

pub(crate) fn require_retirable(store: &StateStore, task: &str) -> Result<(), MergeError> {
    match retirement::require_retirable(store, task) {
        Err(StateError::NotRetirable(reason)) => Err(MergeError::Refused(reason)),
        result => result.map_err(MergeError::from),
    }
}

pub(crate) fn retire_own<Q: PrMerger>(
    store: &mut StateStore,
    config: &Config,
    task: &str,
    prs: &mut Q,
) -> Result<crate::cleanup::CleanupReport, MergeError> {
    require_retirable(store, task)?;
    let merge = pr_merge::merged(store, task)?.ok_or(StateError::LaunchBlocked)?;
    if crate::state::merge_followup::closure(store, task)?.is_none() {
        return Err(StateError::LaunchBlocked.into());
    }
    publish(
        store,
        config,
        task,
        &Retirement {
            phase: RetiredPhase::PrMerged,
            reason: format!(
                "saved verified PR {} merged by Luthor",
                merge.authorization.pr.number
            ),
            actor: None,
            observed_at_unix_secs: now()?,
            evidence: json!({"luthor_merge_evidence":merge}),
        },
        prs,
    )
}

fn settle_held<Q: PrMerger>(
    store: &mut StateStore,
    config: &Config,
    task: &str,
    mut proof: Retirement,
    prs: &mut Q,
) -> Result<Settlement, MergeError> {
    if matches!(proof.phase, RetiredPhase::PrMerged | RetiredPhase::PrClosed) {
        let saved = pr_repair::saved_pr(&store.connection, task)?;
        let detail = prs.terminal_detail(&saved.repository, saved.number)?;
        if proof.phase == RetiredPhase::PrMerged {
            proof = external::proof(store, task, &saved, &detail)?;
        } else {
            validate_identity(&detail, &saved)?;
            if detail["state"] != "closed"
                || detail["merged"] != false
                || detail["closed_at"] != proof.evidence["pr"]["closed_at"]
            {
                return Err(MergeError::Refused("held closed PR proof changed"));
            }
        }
    }
    close_delivered_tracker(store, config, task, &proof, prs)?;
    Ok(settle(store, task, &proof)?)
}

pub(crate) fn publish<Q: PrMerger>(
    store: &mut StateStore,
    config: &Config,
    task: &str,
    proof: &Retirement,
    prs: &mut Q,
) -> Result<crate::cleanup::CleanupReport, MergeError> {
    let cleanup = crate::cleanup::clean_delivered_task(store, config, prs, task)?;
    retirement::retire(store, task, proof)?;
    Ok(cleanup)
}
