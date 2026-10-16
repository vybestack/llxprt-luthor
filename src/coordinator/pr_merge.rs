//! Merging is separate from repair: no head adoption, worker launch or bypass.
use crate::{
    config::{Config, MergePolicy},
    github::pull_request::{PrMerger, merge_validation::validate_identity},
    pr_merge::{MergeAuthorization, MergeEvidence},
    state::{StateError, StateStore, cleanup_records, journal, pr_merge, task_records},
};
pub(crate) mod external;
mod followup;
mod settlement;
use serde_json::Value;
pub(crate) use settlement::{retire_with_tracker, settle_completed_prs};
mod types;
use super::notes::TaskNote;
pub use types::{MergeError, MergeReport};
use types::{Outcome, isolate, now, verify_account};

/// Runs before terminal cleanup so interrupted writes retain their evidence.
/// Disabled mappings are not read or mutated, even with an old merge intent.
pub fn merge_verified_prs<Q: PrMerger>(
    store: &mut StateStore,
    config: &Config,
    prs: &mut Q,
) -> Result<MergeReport, MergeError> {
    let mut report = MergeReport::default();
    if config
        .mappings
        .iter()
        .all(|mapping| mapping.merge.is_disabled())
    {
        return Ok(report);
    }
    for task in cleanup_records::candidates(store)? {
        let selection =
            task_records::selection_evidence(store, &task)?.ok_or(StateError::InvalidSelection)?;
        let Some(mapping) = config
            .mappings
            .iter()
            .find(|mapping| mapping.tracker_repository == selection.candidate.repository)
        else {
            continue;
        };
        if mapping.merge.is_disabled()
            || crate::state::merge_followup::closure(store, &task)?.is_some()
        {
            continue;
        }
        let authorized = pr_merge::authorization(store, &task)?.is_some();
        // Saved authorization pins the merged head. Selection drift must not
        // strand confirmation/closure; it never permits a new merge.
        let outcome = if authorized || mapping.same_worker_mapping(&selection.candidate.mapping) {
            let saved = cleanup_records::verified_pr(store, &task)?;
            if !authorized
                && !crate::state::pr_poll::begin(store, &task, "merge", &saved.head_commit_sha)?
            {
                if let Some(reason) = crate::github::budget::task_deferral_reason(&task) {
                    report.waiting.push(TaskNote {
                        task_id: task,
                        reason,
                    });
                }
                continue;
            }
            let result = inspect_and_finish(store, config, &task, &mapping.merge, prs);
            isolate(store, &task, result)?
        } else {
            let reason = "merge mapping differs from saved selection";
            let facts = serde_json::json!({"reason":reason,"saved":selection.candidate.mapping,"current":config.mappings}).to_string();
            if !crate::state::pr_poll::changed_mapping(store, &task, &facts)? {
                continue;
            }
            Outcome::Waiting(reason.into())
        };
        match outcome {
            Outcome::Merged(mut cleanup) => {
                report.cleanup.append(&mut cleanup);
                report.merged.push(task);
            }
            Outcome::Waiting(reason) => report.waiting.push(TaskNote {
                task_id: task,
                reason,
            }),
            Outcome::Delivered(proof, mut cleanup) => {
                report.cleanup.append(&mut cleanup);
                report.delivered.push(TaskNote {
                    task_id: task,
                    reason: proof.reason,
                });
            }
        }
    }
    Ok(report)
}

fn inspect_and_finish<Q: PrMerger>(
    store: &mut StateStore,
    config: &Config,
    task: &str,
    policy: &MergePolicy,
    prs: &mut Q,
) -> Result<Outcome, MergeError> {
    match inspect(store, task, policy, prs)? {
        Outcome::Merged(_) => {
            followup::close_tracker(store, task, prs)?;
            Ok(Outcome::Merged(settlement::retire_own(
                store, config, task, prs,
            )?))
        }
        Outcome::Delivered(proof, _) => {
            let cleanup = settlement::publish(store, config, task, &proof, prs)?;
            Ok(Outcome::Delivered(proof, cleanup))
        }
        outcome @ Outcome::Waiting(_) => Ok(outcome),
    }
}

fn inspect<Q: PrMerger>(
    store: &mut StateStore,
    task: &str,
    policy: &MergePolicy,
    prs: &mut Q,
) -> Result<Outcome, MergeError> {
    settlement::require_retirable(store, task)?;
    let saved = cleanup_records::verified_pr(store, task)?;
    if pr_merge::completed(store, task)? && pr_merge::authorization(store, task)?.is_none() {
        return Err(MergeError::Refused("merge evidence without authorization"));
    }
    if let Some(auth) = pr_merge::authorization(store, task)? {
        if auth.pr != saved {
            return Err(MergeError::Refused(
                "saved proof changed after merge intent",
            ));
        }
        let detail = prs.terminal_detail(&auth.pr.repository, auth.pr.number)?;
        return confirm(store, task, auth, &detail).map(|()| Outcome::Merged(Default::default()));
    }
    inspect_open(store, task, policy, &saved, prs)
}

fn inspect_open<Q: PrMerger>(
    store: &mut StateStore,
    task: &str,
    policy: &MergePolicy,
    saved: &crate::model::VerifiedOpenPr,
    prs: &mut Q,
) -> Result<Outcome, MergeError> {
    let MergePolicy::Enabled {
        method,
        required_checks,
        allow_draft,
    } = policy
    else {
        return Err(MergeError::Refused("policy disabled"));
    };
    let detail = prs.terminal_detail(&saved.repository, saved.number)?;
    if detail["state"] == "closed" || detail["merged"] == true {
        return settlement::finish(store, task, saved, &detail, true, prs);
    }
    if followup::updated_head(store, task, saved, &detail)? {
        return Ok(Outcome::Waiting(
            "base update head awaits existing PR repair verification".into(),
        ));
    }
    validate_identity(&detail, saved)?;
    let eligibility_reason = eligibility(&detail, *allow_draft)?;
    if detail["state"] != "open" || (detail["draft"] == true && !allow_draft) {
        return Ok(eligibility_reason.map_or(Outcome::Merged(Default::default()), Outcome::Waiting));
    }
    verify_account(prs, saved)?;
    let checks = prs.merge_checks(&saved.repository, saved.number, required_checks)?;
    if checks.health.id != saved.id || checks.health.head != saved.head_commit_sha {
        return Err(MergeError::Refused("check head differs from verified PR"));
    }
    if let Some(reason) = check_gate(store, task, saved, &checks)? {
        return Ok(Outcome::Waiting(reason));
    }
    if detail["mergeable_state"] == "behind" && detail["mergeable"] == true {
        return followup::update_branch(store, task, saved, prs).map(Outcome::Waiting);
    }
    if let Some(reason) = eligibility_reason {
        return Ok(Outcome::Waiting(reason));
    }
    let auth = MergeAuthorization {
        pr: saved.clone(),
        policy: policy.clone(),
        checks,
        observed_at: now()?,
    };
    // Durable before either mutation, including draft promotion.
    pr_merge::begin(store, task, &auth)?;
    if detail["draft"] == true {
        prs.ready_for_review(saved)?;
    }
    merge_authorized(store, task, auth, *method, prs)?;
    Ok(Outcome::Merged(Default::default()))
}

fn merge_authorized<Q: PrMerger>(
    store: &mut StateStore,
    task: &str,
    auth: MergeAuthorization,
    method: crate::config::MergeMethod,
    prs: &mut Q,
) -> Result<(), MergeError> {
    // Draft promotion or concurrent edits may change eligibility. Recheck, never
    // replace the authorized head with a newer one.
    let detail = prs.terminal_detail(&auth.pr.repository, auth.pr.number)?;
    validate_identity(&detail, &auth.pr)?;
    if eligibility(&detail, false)?.is_some() {
        return Err(MergeError::Refused("PR no longer eligible"));
    }
    let commit = prs.merge_head(&auth.pr, method)?;
    let detail = prs.terminal_detail(&auth.pr.repository, auth.pr.number)?;
    if detail["merge_commit_sha"].as_str() != Some(&commit) {
        return Err(MergeError::Refused(
            "merge response and terminal commit disagree",
        ));
    }
    confirm(store, task, auth, &detail)
}

fn confirm(
    store: &mut StateStore,
    task: &str,
    auth: MergeAuthorization,
    detail: &Value,
) -> Result<(), MergeError> {
    validate_identity(detail, &auth.pr)?;
    if detail["state"] != "closed"
        || detail["merged"] != true
        || !detail["merged_at"]
            .as_str()
            .is_some_and(|time| !time.is_empty())
    {
        return Err(MergeError::Refused(
            "merge intent unresolved; no repeated write authorized",
        ));
    }
    let commit = detail["merge_commit_sha"]
        .as_str()
        .filter(|sha| valid_sha(sha))
        .ok_or(MergeError::Refused("merge commit missing"))?;
    let evidence = MergeEvidence {
        merged_head: auth.pr.head_commit_sha.clone(),
        authorization: auth,
        merge_commit: commit.into(),
        observed_at: now()?,
    };
    if let Some(prior) = pr_merge::merged(store, task)? {
        if prior.authorization != evidence.authorization
            || prior.merge_commit != evidence.merge_commit
            || prior.merged_head != evidence.merged_head
        {
            return Err(MergeError::Refused("terminal merge proof changed"));
        }
    } else {
        pr_merge::complete(store, task, &evidence)?;
    }
    Ok(())
}

fn eligibility(detail: &Value, allow_draft: bool) -> Result<Option<String>, MergeError> {
    let draft = detail["draft"]
        .as_bool()
        .ok_or(MergeError::Refused("draft state unavailable"))?;
    let state = detail["state"]
        .as_str()
        .filter(|state| matches!(*state, "open" | "closed"))
        .ok_or(MergeError::Refused("PR state unavailable"))?;
    let merged = detail["merged"]
        .as_bool()
        .ok_or(MergeError::Refused("merge state unavailable"))?;
    if state != "open" || merged {
        return Ok(Some("PR is not open and unmerged".into()));
    }
    if draft && !allow_draft {
        return Ok(Some("draft PR excluded by merge policy".into()));
    }
    let mergeable = detail
        .get("mergeable")
        .filter(|value| value.is_null() || value.is_boolean())
        .ok_or(MergeError::Refused("mergeability unavailable"))?;
    let mergeable_state = detail["mergeable_state"]
        .as_str()
        .ok_or(MergeError::Refused("mergeability state unavailable"))?;
    let clean = mergeable_state == "clean"
        || (draft && allow_draft && detail["mergeable_state"] == "draft");
    if mergeable != true || !clean {
        return Ok(Some("PR is not MERGEABLE and CLEAN".into()));
    }
    Ok(None)
}

fn valid_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

fn check_gate(
    store: &mut StateStore,
    task: &str,
    saved: &crate::model::VerifiedOpenPr,
    checks: &crate::github::pull_request::MergeChecks,
) -> Result<Option<String>, MergeError> {
    journal::record_evidence(
        store,
        task,
        Some(&saved.attempt_id),
        "pr_health",
        &serde_json::to_string(&checks.health).map_err(StateError::from)?,
    )?;
    journal::record_evidence(
        store,
        task,
        Some(&saved.attempt_id),
        "pr_merge_checks",
        &serde_json::to_string(&checks).map_err(StateError::from)?,
    )?;
    if !checks.health.failed_required_checks.is_empty() {
        return Ok(Some(format!(
            "failing checks: {}",
            checks.health.failed_required_checks.join(", ")
        )));
    }
    if checks.health.required_checks_pending || checks.health.conflicting {
        if checks.health.required_checks_pending {
            crate::github::budget::defer_task(task);
        }
        return Ok(Some("required checks pending or PR conflicting".into()));
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
