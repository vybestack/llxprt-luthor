use super::{
    context::{
        KIND, NeverDispatchedAuthorization, NeverDispatchedContext, NeverDispatchedReason,
        SEAL_KIND, SavedClaimAssignment, SavedRows, valid_actor,
    },
    proofs::{dispatch_evidence, dispatch_intent, parse_saved},
};
use crate::model::{SelectionEvidence, StateError, WorktreeIdentity, WorktreeIntent};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;
use std::time::SystemTime;

pub(crate) type ProofRow = (String, Option<String>, String, String);

fn strip_dispatch_rows(
    rows: &mut Vec<ProofRow>,
    evidence: bool,
    task: &str,
    attempt: &str,
) -> Result<(), StateError> {
    let allowed = |kind: &str| {
        if evidence {
            dispatch_evidence(kind)
        } else {
            dispatch_intent(kind)
        }
    };
    if rows
        .iter()
        .any(|row| allowed(&row.2) && (row.0 != task || row.1.as_deref() != Some(attempt)))
    {
        return Err(StateError::LaunchBlocked);
    }
    rows.retain(|row| !allowed(&row.2));
    Ok(())
}

#[derive(Serialize)]
struct AuthorizationAudit<'a> {
    actor: &'a str,
    reason_code: NeverDispatchedReason,
    authorized_at_unix_secs: u64,
    task_id: &'a str,
    attempt_id: &'a str,
    saved_launch_plan: &'a str,
    selection: &'a SelectionEvidence,
    claim: &'a SavedClaimAssignment,
    worktree_intent: &'a WorktreeIntent,
    worktree_identity: &'a WorktreeIdentity,
}

pub(crate) fn authorize(
    connection: &mut Connection,
    root: &std::path::Path,
    context: &NeverDispatchedContext,
    actor: &str,
    reason: NeverDispatchedReason,
) -> Result<NeverDispatchedAuthorization, StateError> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let fresh = read_context(&tx, root, context.task_id(), context.attempt_id())?;
    if fresh != *context || fresh.amendment.is_some() || !valid_actor(actor, &fresh) {
        return Err(StateError::LaunchBlocked);
    }
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_secs();
    if timestamp == 0 {
        return Err(StateError::LaunchBlocked);
    }
    let audit = AuthorizationAudit {
        actor,
        reason_code: reason,
        authorized_at_unix_secs: timestamp,
        task_id: fresh.task_id(),
        attempt_id: fresh.attempt_id(),
        saved_launch_plan: fresh.saved_launch_plan(),
        selection: fresh.selection(),
        claim: fresh.claim(),
        worktree_intent: fresh.worktree_intent(),
        worktree_identity: fresh.worktree_identity(),
    };
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'never_dispatched_authorized',?3)",
        params![fresh.task_id(), fresh.attempt_id(), serde_json::to_string(&audit)?],
    )?;
    let audit_sequence = tx.last_insert_rowid();
    tx.commit()?;
    Ok(NeverDispatchedAuthorization {
        context: fresh,
        audit_sequence,
    })
}

pub(crate) fn read_context(
    db: &Connection,
    root: &std::path::Path,
    task_id: &str,
    attempt_id: &str,
) -> Result<NeverDispatchedContext, StateError> {
    read_context_mode(db, root, task_id, attempt_id, false)
}

pub(crate) fn read_context_mode(
    db: &Connection,
    root: &std::path::Path,
    task_id: &str,
    attempt_id: &str,
    dispatched: bool,
) -> Result<NeverDispatchedContext, StateError> {
    validate_attempt(db, task_id, attempt_id)?;
    let snapshot = saved_rows(db, task_id, attempt_id, dispatched)?;
    let mut intents = proof_rows(db, false, task_id, attempt_id)?;
    let mut evidence = proof_rows(db, true, task_id, attempt_id)?;
    if dispatched {
        strip_dispatch_rows(&mut intents, false, task_id, attempt_id)?;
        strip_dispatch_rows(&mut evidence, true, task_id, attempt_id)?;
    }
    assemble_context(db, root, task_id, attempt_id, snapshot, &intents, &evidence)
}

pub(crate) fn assemble_context(
    db: &Connection,
    root: &std::path::Path,
    task_id: &str,
    attempt_id: &str,
    snapshot: SavedRows,
    intents: &[ProofRow],
    evidence: &[ProofRow],
) -> Result<NeverDispatchedContext, StateError> {
    validate_proof_scope(intents, evidence, task_id, attempt_id)?;
    let saved_launch_plan = required_payload(intents, "launch")?.to_owned();
    let mut context = NeverDispatchedContext {
        root: root.to_owned(),
        snapshot,
        amendment: None,
        plan: parse_saved(&saved_launch_plan)?,
        saved_launch_plan,
        selection: parse_saved(required_payload(evidence, "selection")?)?,
        claim: parse_saved(required_payload(intents, "claim_assignment")?)?,
        claim_verified: required_payload(evidence, "claim_verified")?.to_owned(),
        worktree_intent: parse_saved(required_payload(intents, "worktree_create")?)?,
        worktree_identity: parse_saved(required_payload(evidence, "worktree_created")?)?,
    };
    validate_saved_identity(db, &context, task_id, attempt_id)?;
    context.amendment = super::amendment::read_audit(db, &context)?;
    Ok(context)
}

fn validate_attempt(db: &Connection, task: &str, attempt: &str) -> Result<(), StateError> {
    if !valid_identifier(task) || !valid_identifier(attempt) {
        return Err(StateError::LaunchBlocked);
    }
    let valid: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks t JOIN attempts a ON a.task_id=t.id
         JOIN reservations r ON r.task_id=t.id AND r.attempt_id=a.id
         WHERE t.id=?1 AND t.state='held' AND a.id=?2
           AND a.lifecycle='launch_intended' AND a.outcome IS NULL AND r.status='reserved'
           AND (SELECT COUNT(*) FROM attempts WHERE task_id=?1)=1
           AND (SELECT COUNT(*) FROM reservations WHERE task_id=?1 OR attempt_id=?2)=1)",
        params![task, attempt],
        |row| row.get(0),
    )?;
    if !valid {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

pub(crate) fn proof_rows(
    db: &Connection,
    evidence: bool,
    task: &str,
    attempt: &str,
) -> Result<Vec<ProofRow>, StateError> {
    let sql = if evidence {
        "SELECT task_id,attempt_id,kind,payload FROM evidence WHERE task_id=?1 OR attempt_id=?2 ORDER BY sequence"
    } else {
        "SELECT task_id,attempt_id,kind,detail FROM intents WHERE task_id=?1 OR attempt_id=?2 ORDER BY sequence"
    };
    Ok(db
        .prepare(sql)?
        .query_map(params![task, attempt], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?)
}

fn validate_proof_scope(
    intents: &[ProofRow],
    evidence: &[ProofRow],
    task: &str,
    attempt: &str,
) -> Result<(), StateError> {
    let intent_kinds: Vec<_> = intents
        .iter()
        .filter(|r| r.2 != SEAL_KIND)
        .map(|r| r.2.as_str())
        .collect();
    let evidence_kinds: Vec<_> = evidence
        .iter()
        .filter(|r| r.2 != "held_reason" && r.2 != KIND)
        .map(|r| r.2.as_str())
        .collect();
    if intent_kinds != ["claim_assignment", "worktree_create", "launch"]
        || evidence_kinds != ["selection", "claim_verified", "worktree_created"]
        || intents.iter().any(|r| {
            r.0 != task
                || r.1.as_deref()
                    != if r.2 == "launch" || r.2 == SEAL_KIND {
                        Some(attempt)
                    } else {
                        None
                    }
        })
        || evidence.iter().any(|r| {
            r.0 != task
                || if r.2 == KIND {
                    r.1.as_deref() != Some(attempt)
                } else {
                    r.1.is_some()
                }
        })
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

fn required_payload<'a>(rows: &'a [ProofRow], kind: &str) -> Result<&'a str, StateError> {
    rows.iter()
        .find(|r| r.2 == kind)
        .map(|r| r.3.as_str())
        .ok_or(StateError::LaunchBlocked)
}

// Typed deserialization rejects repeated known fields. Comparing the serialized
// shape rejects ignored/unknown fields throughout the saved object graph.

fn validate_saved_identity(
    db: &Connection,
    c: &NeverDispatchedContext,
    task: &str,
    attempt: &str,
) -> Result<(), StateError> {
    let selection = &c.selection;
    let candidate = &selection.candidate;
    let config = &selection.effective_config;
    let mapping = &candidate.mapping;
    let wt = &c.worktree_identity;
    let plan = &c.plan;
    let consistent: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND tracker_repo_id=?2 AND issue_node_id=?3
         AND repository=?4 AND issue_number=?5 AND config_revision=?6
         AND (SELECT value FROM state_meta WHERE key='capacity')=?7)",
        params![
            task,
            candidate.tracker_repo_id,
            candidate.issue_node_id,
            candidate.repository,
            candidate.issue_number,
            selection.config_revision,
            config.capacity
        ],
        |row| row.get(0),
    )?;
    if !consistent
        || !valid_selection_identity(selection)
        || plan.task_id != task
        || plan.attempt_id != attempt
        || plan.session_id != task
        || plan.config_revision != selection.config_revision
        || selection.config_revision.trim().is_empty()
        || c.claim.principal != config.assignment_login
        || c.claim_verified != c.claim.principal
        || c.claim.repository != candidate.repository
        || c.claim.number != candidate.issue_number
        || c.claim.principal.trim().is_empty()
        || candidate.issue_number == 0
        || !config.sources.contains(&candidate.source)
        || !config.mappings.contains(mapping)
        || candidate.project_id != candidate.source.project_id
        || !candidate
            .source
            .repositories
            .contains(&candidate.repository)
        || candidate.repository != mapping.tracker_repository
        || candidate.marker != candidate.source.ready_marker
        || candidate.issue_url
            != format!(
                "https://github.com/{}/issues/{}",
                candidate.repository, candidate.issue_number
            )
        || candidate
            .source
            .milestone
            .as_ref()
            .is_some_and(|m| candidate.milestone_title.as_ref() != Some(m))
    {
        return Err(StateError::LaunchBlocked);
    }
    validate_worktree_plan(c, wt, mapping)
}

fn valid_selection_identity(selection: &SelectionEvidence) -> bool {
    let candidate = &selection.candidate;
    let config = &selection.effective_config;
    config.capacity > 0
        && [
            &candidate.project_id,
            &candidate.item_id,
            &candidate.issue_node_id,
            &candidate.tracker_repo_id,
        ]
        .into_iter()
        .all(|value| !value.trim().is_empty())
        && candidate.observed_at_unix_secs > 0
        && candidate.observed_state == "open"
        && candidate.observed_assignees.is_empty()
}

fn validate_worktree_plan(
    c: &NeverDispatchedContext,
    wt: &WorktreeIdentity,
    mapping: &crate::config::Mapping,
) -> Result<(), StateError> {
    let intent = &c.worktree_intent;
    let plan = &c.plan;
    let env = &plan.session_environment;
    if intent.path != wt.path
        || intent.branch != wt.branch
        || intent.base != wt.base
        || intent.repository != wt.repository
        || wt.base != mapping.base_branch
        || wt.repository != mapping.code_repository
        || wt.branch != format!("luthor/{}", plan.task_id)
        || wt
            .path
            .file_name()
            .is_none_or(|name| name != plan.task_id.as_str())
        || !wt.path.is_absolute()
        || !wt.git_directory.is_absolute()
        || wt.head.trim().is_empty()
        || wt.remote.trim().is_empty()
        || plan.worktree != wt.path
        || plan.expected_worktree != *wt
        || plan.executable != c.selection.effective_config.initial.executable
        || plan.executable.as_os_str().is_empty()
        || !env.home.is_absolute()
        || [
            &env.xdg_config_home,
            &env.xdg_data_home,
            &env.xdg_state_home,
            &env.llxprt_config_home,
        ]
        .into_iter()
        .flatten()
        .any(|p| !p.is_absolute())
        || !exact_pair(&plan.args, "--session", &plan.session_id)
        || !exact_pair(&plan.args, "--cwd", &wt.path.to_string_lossy())
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

fn exact_pair(args: &[String], flag: &str, value: &str) -> bool {
    let positions: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| arg.as_str() == flag || arg.starts_with(&format!("{flag}=")))
        .map(|(i, _)| i)
        .collect();
    matches!(positions.as_slice(), [index] if args[*index] == flag && args.get(index + 1).is_some_and(|v| v == value))
}

pub(crate) fn saved_rows(
    db: &Connection,
    task: &str,
    attempt: &str,
    dispatched: bool,
) -> Result<SavedRows, StateError> {
    let queries = [
        "SELECT json_array(rowid,id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision,created_at) FROM tasks WHERE id=?1 ORDER BY rowid",
        "SELECT json_array(rowid,id,task_id,lifecycle,outcome,created_at) FROM attempts WHERE task_id=?1 OR id=?2 ORDER BY rowid",
        "SELECT json_array(rowid,attempt_id,task_id,status,created_at) FROM reservations WHERE task_id=?1 OR attempt_id=?2 ORDER BY rowid",
        "SELECT json_array(sequence,id,task_id,attempt_id,kind,detail,created_at) FROM intents WHERE (task_id=?1 OR attempt_id=?2) AND kind!='initial_branch_removal_seal' AND (?3=0 OR kind NOT IN ('supervisor_dispatch','gate_release')) ORDER BY sequence",
        "SELECT json_array(sequence,task_id,attempt_id,kind,payload,created_at) FROM evidence WHERE (task_id=?1 OR attempt_id=?2) AND kind!='initial_branch_removed' AND (?3=0 OR kind NOT IN ('supervisor_ready','child_registered','tracked_descendant','gate_sent')) ORDER BY sequence",
    ];
    let mut rows = Vec::new();
    for (index, query) in queries.iter().enumerate() {
        let mut stmt = db.prepare(query)?;
        let mut result = if index == 0 {
            stmt.query([task])?
        } else if index >= 3 {
            stmt.query(params![task, attempt, dispatched])?
        } else {
            stmt.query(params![task, attempt])?
        };
        let mut table = Vec::new();
        while let Some(row) = result.next()? {
            table.push(row.get(0)?);
        }
        rows.push(table);
    }
    Ok(SavedRows(rows))
}
