use super::ports::observation_time;
use crate::state::journal;
use crate::{
    model::{ExitPrEvidence, PausePrStatus, StateError, VerifiedOpenPr},
    state::{OperatorRecoveryAudit, StateStore, exits},
    supervisor::SupervisorError,
};
use rusqlite::Connection;
use serde_json::Value;

pub(crate) enum RecoveryPrEvidence {
    Absent,
    Open(Box<VerifiedOpenPr>),
}

struct ProcessIdentities {
    supervisor: Value,
    child: Value,
    tracked: Vec<Value>,
}

pub(crate) fn audited_commit(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    actor: &str,
    reason: &str,
    repository: &str,
    pr: RecoveryPrEvidence,
) -> Result<RecoveryResult, SupervisorError> {
    let Some(identities) = process_identities(store, task_id, attempt_id)? else {
        return Ok(RecoveryResult::Held(
            "process identity evidence is missing".into(),
        ));
    };
    let observed_at_unix_secs = observation_time()?;
    if observed_at_unix_secs == 0 {
        return Ok(RecoveryResult::Held("invalid recovery timestamp".into()));
    }
    let audit = identities.audit(actor, reason, observed_at_unix_secs)?;
    let (status, proof) = match pr {
        RecoveryPrEvidence::Absent => (PausePrStatus::Absent, None),
        RecoveryPrEvidence::Open(proof) => (PausePrStatus::Open, Some(*proof)),
    };
    let lookup = ExitPrEvidence {
        observed_at_unix_secs,
        repository: repository.to_owned(),
        status,
    };
    Ok(commit(
        &mut store.connection,
        task_id,
        attempt_id,
        &audit,
        &lookup,
        proof,
    )?)
}

fn process_identities(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<Option<ProcessIdentities>, SupervisorError> {
    let supervisor =
        journal::evidence_payload(store, task_id, Some(attempt_id), "supervisor_ready")?
            .and_then(|p| serde_json::from_str::<Value>(&p).ok());
    let child = journal::evidence_payload(store, task_id, Some(attempt_id), "child_registered")?
        .and_then(|p| serde_json::from_str::<Value>(&p).ok());
    let tracked = journal::evidence_payloads(store, task_id, attempt_id, "tracked_descendant")?
        .into_iter()
        .map(|p| serde_json::from_str::<Value>(&p))
        .collect::<Result<Vec<_>, _>>()?;
    let (Some(supervisor), Some(child)) = (supervisor, child) else {
        return Ok(None);
    };
    Ok(Some(ProcessIdentities {
        supervisor,
        child,
        tracked,
    }))
}

impl ProcessIdentities {
    fn audit(
        self,
        actor: &str,
        reason: &str,
        observed_at_unix_secs: u64,
    ) -> Result<String, serde_json::Error> {
        let mut os_ids = vec![self.supervisor.clone(), self.child.clone()];
        os_ids.extend(self.tracked.iter().cloned());
        serde_json::to_string(&OperatorRecoveryAudit {
            actor,
            reason,
            observed_at_unix_secs,
            os_ids,
            supervisor: self.supervisor,
            child: self.child,
            tracked_os_identities: self.tracked,
        })
    }
}

fn commit(
    connection: &mut Connection,
    task_id: &str,
    attempt_id: &str,
    audit: &str,
    lookup: &ExitPrEvidence,
    proof: Option<VerifiedOpenPr>,
) -> Result<RecoveryResult, StateError> {
    if let Some(proof) = proof {
        let pr_id = proof.id;
        exits::commit_telemetry_lost_pr_completion(
            connection, task_id, attempt_id, audit, lookup, &proof,
        )?;
        Ok(RecoveryResult::RecoveredPrComplete { pr_id })
    } else {
        exits::commit_telemetry_lost_recovery(connection, task_id, attempt_id, audit, lookup)?;
        Ok(RecoveryResult::RecoveredHeld)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryResult {
    RecoveredHeld,
    RecoveredPrComplete { pr_id: u64 },
    Held(String),
}
