mod amended_completion;
mod amended_completion_lookup;
pub(crate) mod amended_dispatch;
mod amended_observation;
mod amendment;
mod attempts;
mod branch_removal;
mod context;
mod continuation;
mod continuation_hold;
mod dispatch_stages;
pub(crate) mod exits;
mod proofs;
pub(crate) use crate::model::OperatorRecoveryAudit;
pub use crate::model::{
    EffectiveConfigSnapshot, ExitPrEvidence, ExitReceipt, LaunchPlan, PausePrEvidence,
    PausePrStatus, Reconciliation, RecoveryInspection, RetryAuthorization, RetrySourceEvidence,
    SelectionEvidence, SessionEnvironment, StateError, TerminalExitBasis, TerminalExitProof,
    WorktreeIdentity, WorktreeIntent, WorktreeRecord,
};
pub use amended_dispatch::{verify_amended_observation_plan, verify_amended_worker_plan};
pub(crate) use amendment::policy::{initial_branch_removal_plan, validate_removal_config};
pub(crate) use attempts::attempt_selection;
pub(crate) use attempts::same_task_config;
use attempts::{retry_context, retry_evidence_matches};
pub use context::{
    AmendedDispatchProof, BranchRemovalDelta, BranchRemovalReason, BranchRemovalRequest,
    FutureTemplateCorrection, InitialBranchRemovalAudit, ProcessQuiescence,
};
pub use context::{
    NeverDispatchedAuthorization, NeverDispatchedContext, NeverDispatchedReason,
    SavedClaimAssignment,
};

use crate::{config::Config, eligibility::Candidate, pr_evidence::VerifiedOpenPr};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

fn validate_selection(candidate: &Candidate, config: &Config) -> Result<(), StateError> {
    let source = &candidate.source;
    let mapping = &candidate.mapping;
    let identity_matches = !candidate.project_id.is_empty()
        && !candidate.item_id.is_empty()
        && !candidate.issue_node_id.is_empty()
        && !candidate.tracker_repo_id.is_empty()
        && candidate.issue_number > 0
        && candidate.issue_url
            == format!(
                "https://github.com/{}/issues/{}",
                candidate.repository, candidate.issue_number
            );
    let milestone_matches = source
        .milestone
        .as_ref()
        .is_none_or(|title| candidate.milestone_title.as_ref() == Some(title));
    if !config.sources.contains(source)
        || !config.mappings.contains(mapping)
        || candidate.project_id != source.project_id
        || !source.repositories.contains(&candidate.repository)
        || candidate.repository != mapping.tracker_repository
        || candidate.marker != source.ready_marker
        || !milestone_matches
        || !identity_matches
    {
        return Err(StateError::InvalidSelection);
    }
    Ok(())
}

struct StateLock(File);

impl Drop for StateLock {
    fn drop(&mut self) {
        self.0.unlock().expect("failed to release state lock");
    }
}

pub struct StateStore {
    pub(crate) connection: Connection,
    _lock: StateLock,
    root: PathBuf,
}

fn resume_context(
    connection: &Connection,
    task_id: &str,
) -> Result<(String, String, String), StateError> {
    continuation_hold::require_unamended_task(connection, task_id)?;
    let latest: Option<(String, String, String)> = connection.query_row(
        "SELECT a.id, i.detail, e.payload FROM attempts a
         JOIN tasks t ON t.id=a.task_id
         JOIN reservations r ON r.attempt_id=a.id AND r.task_id=a.task_id
         JOIN intents i ON i.attempt_id=a.id AND i.task_id=a.task_id AND i.kind='launch'
         JOIN evidence e ON e.attempt_id=a.id AND e.task_id=a.task_id AND e.kind='attempt_exit'
         WHERE a.task_id=?1 AND t.state='paused' AND a.lifecycle='completed'
           AND a.outcome IS NOT NULL AND r.status='released'
           AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=a.id AND kind='attempt_exit')=1
           AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=a.id AND task_id=?1 AND kind='pause_pr_lookup' AND json_extract(payload,'$.status.status')='absent')=1
           AND (SELECT COUNT(*) FROM intents WHERE attempt_id=a.id AND kind='launch')=1
           AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=a.id AND kind='stop')=1
           AND (SELECT COUNT(*) FROM reservations WHERE task_id=?1 AND status='reserved')=0
           AND (SELECT COUNT(*) FROM attempts WHERE task_id=?1 AND (lifecycle!='completed' OR outcome IS NULL))=0
           AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='claim_verified')=1
           AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='worktree_created')=1
         ORDER BY a.rowid DESC LIMIT 1",
        [task_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    ).optional()?;
    let (latest_id, latest_plan, exit) = latest.ok_or(StateError::LaunchBlocked)?;
    let actual_latest: Option<String> = connection
        .query_row(
            "SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    if actual_latest.as_deref() != Some(&latest_id) {
        return Err(StateError::LaunchBlocked);
    }
    let initial: Vec<String> = connection
        .prepare(
            "SELECT i.detail FROM attempts a JOIN intents i ON i.attempt_id=a.id
         WHERE a.task_id=?1 AND i.task_id=?1 AND i.kind='launch'
         ORDER BY a.rowid LIMIT 1",
        )?
        .query_map([task_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let first_plan = initial
        .into_iter()
        .next()
        .ok_or(StateError::LaunchBlocked)?;
    let receipt: crate::model::ExitReceipt = serde_json::from_str(&exit)?;
    let outcome: String = connection.query_row(
        "SELECT outcome FROM attempts WHERE id=?1",
        [&latest_id],
        |row| row.get(0),
    )?;
    if receipt.attempt_id != latest_id
        || receipt.stop_signals.is_empty()
        || outcome
            != format!(
                "exit_code={:?};signal={:?}",
                receipt.exit_code, receipt.signal
            )
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok((first_plan, latest_plan, exit))
}

impl NeverDispatchedContext {
    /// Recheck the exact saved rows and commit the audit before any continuation
    /// side effects. External read-only validation remains the caller's duty.
    pub fn authorize(
        &self,
        store: &mut StateStore,
        actor: &str,
        reason: NeverDispatchedReason,
    ) -> Result<NeverDispatchedAuthorization, StateError> {
        continuation::authorize(&mut store.connection, &store.root, self, actor, reason)
    }
}

impl StateStore {
    pub fn authorize_initial_branch_removal(
        &mut self,
        context: &NeverDispatchedContext,
        effective: &LaunchPlan,
        request: BranchRemovalRequest<'_>,
    ) -> Result<i64, StateError> {
        branch_removal::authorize_initial_branch_removal(
            &mut self.connection,
            &self.root,
            context,
            effective,
            request,
        )
    }

    pub fn amended_dispatch_proof(
        &self,
        context: &NeverDispatchedContext,
        config: &Config,
        revision: &str,
    ) -> Result<AmendedDispatchProof, StateError> {
        amended_dispatch::amended_dispatch_proof(
            &self.connection,
            &self.root,
            context,
            config,
            revision,
        )
    }

    pub fn begin_amended_supervision(
        &mut self,
        context: &NeverDispatchedContext,
        config: &Config,
        revision: &str,
    ) -> Result<AmendedDispatchProof, StateError> {
        amended_dispatch::begin_amended_supervision(
            &mut self.connection,
            &self.root,
            context,
            config,
            revision,
        )
    }

    pub fn open(root: impl AsRef<Path>, capacity: usize) -> Result<Self, StateError> {
        if capacity == 0 {
            return Err(StateError::InvalidCapacity);
        }
        let root = root.as_ref();
        fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("coordinator.lock"))?;
        lock.try_lock_exclusive().map_err(StateError::Io)?;
        let mut connection = Connection::open(root.join("state.sqlite3"))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let version: i32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 3 {
            return Err(StateError::UnsupportedDatabaseVersion(version));
        }
        let tx = connection.transaction()?;
        if version == 0 {
            tx.execute_batch("CREATE TABLE tasks (id TEXT PRIMARY KEY, tracker_repo_id TEXT NOT NULL, issue_node_id TEXT NOT NULL, repository TEXT NOT NULL, issue_number INTEGER NOT NULL, state TEXT NOT NULL, config_revision TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(tracker_repo_id, issue_node_id));
                CREATE TABLE attempts (id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), lifecycle TEXT NOT NULL, outcome TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                CREATE TABLE intents (sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                CREATE TABLE reservations (attempt_id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), status TEXT NOT NULL CHECK(status IN ('reserved','released')), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                CREATE UNIQUE INDEX reservations_one_active_per_task ON reservations(task_id) WHERE status='reserved';
                CREATE TABLE evidence (sequence INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);")?;
        }
        if version < 2 {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS state_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);")?;
            tx.execute(
                "INSERT INTO state_meta(key,value) VALUES('capacity',?1)",
                [capacity as i64],
            )?;
            tx.pragma_update(None, "user_version", 2)?;
        } else {
            let persisted: i64 = tx.query_row(
                "SELECT value FROM state_meta WHERE key='capacity'",
                [],
                |row| row.get(0),
            )?;
            if persisted != capacity as i64 {
                return Err(StateError::CapacityMismatch {
                    configured: capacity,
                    persisted: persisted as usize,
                });
            }
        }
        if version < 3 {
            tx.execute_batch("CREATE TABLE reservations_v3 (attempt_id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), status TEXT NOT NULL CHECK(status IN ('reserved','released')), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                INSERT INTO reservations_v3(attempt_id,task_id,status,created_at) SELECT attempt_id,task_id,status,created_at FROM reservations;
                DROP TABLE reservations;
                ALTER TABLE reservations_v3 RENAME TO reservations;
                CREATE UNIQUE INDEX reservations_one_active_per_task ON reservations(task_id) WHERE status='reserved';")?;
            tx.pragma_update(None, "user_version", 3)?;
        }
        tx.commit()?;
        Ok(Self {
            connection,
            _lock: StateLock(lock),
            root: root.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Inspect one held, first-and-latest saved launch and reservation without
    /// reading files, observing processes or GitHub, or changing state.
    pub fn never_dispatched_context(
        &self,
        task_id: &str,
        attempt_id: &str,
    ) -> Result<NeverDispatchedContext, StateError> {
        let tx = self.connection.unchecked_transaction()?;
        let context = continuation::read_context(&tx, &self.root, task_id, attempt_id)?;
        tx.commit()?;
        Ok(context)
    }

    pub fn claimed_worktree_context(&self, task_id: &str) -> Result<SelectionEvidence, StateError> {
        let phase: Option<String> = self
            .connection
            .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let verified: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='claim_verified'",
            [task_id],
            |row| row.get(0),
        )?;
        let claim: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND kind='claim_assignment'",
            [task_id],
            |row| row.get(0),
        )?;
        if phase.as_deref() != Some("claimed") || verified != 1 || claim != 1 {
            return Err(StateError::InvalidSelection);
        }
        self.selection_evidence(task_id)?
            .ok_or(StateError::InvalidSelection)
    }

    pub fn worktree_record(&self, task_id: &str) -> Result<Option<WorktreeRecord>, StateError> {
        let mut intents = self.connection.prepare(
            "SELECT detail FROM intents WHERE task_id=?1 AND kind='worktree_create' ORDER BY sequence",
        )?;
        let details = intents
            .query_map([task_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if details.len() > 1 {
            return Err(StateError::InvalidSelection);
        }
        let Some(detail) = details.first() else {
            return Ok(None);
        };
        let intent: WorktreeIntent = serde_json::from_str(detail)?;
        let mut evidence = self.connection.prepare(
            "SELECT payload FROM evidence WHERE task_id=?1 AND kind='worktree_created' ORDER BY sequence",
        )?;
        let payloads = evidence
            .query_map([task_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if payloads.len() > 1 {
            return Err(StateError::InvalidSelection);
        }
        let identity = payloads
            .first()
            .map(|payload| serde_json::from_str(payload))
            .transpose()?;
        Ok(Some(WorktreeRecord { intent, identity }))
    }

    pub fn begin_worktree(
        &mut self,
        task_id: &str,
        intent: &WorktreeIntent,
    ) -> Result<(), StateError> {
        let tx = self.connection.transaction()?;
        let phase: Option<String> = tx
            .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let verified: i64 = tx.query_row(
            "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='claim_verified'",
            [task_id],
            |row| row.get(0),
        )?;
        let claimed: i64 = tx.query_row(
            "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND kind='claim_assignment'",
            [task_id],
            |row| row.get(0),
        )?;
        let previous: i64 = tx.query_row(
            "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND kind='worktree_create'",
            [task_id],
            |row| row.get(0),
        )?;
        if phase.as_deref() != Some("claimed") || verified != 1 || claimed != 1 || previous != 0 {
            return Err(StateError::InvalidSelection);
        }
        tx.execute(
            "INSERT INTO intents(id,task_id,kind,detail) VALUES(?1,?2,'worktree_create',?3)",
            params![
                format!("worktree-{task_id}"),
                task_id,
                serde_json::to_string(intent)?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish_worktree(
        &mut self,
        task_id: &str,
        identity: &WorktreeIdentity,
    ) -> Result<(), StateError> {
        let tx = self.connection.transaction()?;
        let intent: Option<String> = tx
            .query_row(
                "SELECT detail FROM intents WHERE task_id=?1 AND kind='worktree_create'",
                [task_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(intent) = intent else {
            return Err(StateError::InvalidSelection);
        };
        let intent: WorktreeIntent = serde_json::from_str(&intent)?;
        let existing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='worktree_created'",
            [task_id],
            |row| row.get(0),
        )?;
        if existing != 0
            || intent.path != identity.path
            || intent.branch != identity.branch
            || intent.base != identity.base
            || intent.repository != identity.repository
        {
            return Err(StateError::InvalidSelection);
        }
        tx.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,NULL,'worktree_created',?2)",
            params![task_id, serde_json::to_string(identity)?])?;
        tx.commit()?;
        Ok(())
    }

    pub fn create_task(
        &mut self,
        id: &str,
        candidate: &Candidate,
        config_revision: &str,
        config: &Config,
    ) -> Result<(), StateError> {
        config.validate().map_err(|_| StateError::InvalidConfig)?;
        validate_selection(candidate, config)?;
        let repo_id = &candidate.tracker_repo_id;
        let issue_node_id = &candidate.issue_node_id;
        let tx = self.connection.transaction()?;
        let exists: Option<String> = tx
            .query_row(
                "SELECT id FROM tasks WHERE tracker_repo_id=?1 AND issue_node_id=?2",
                params![repo_id, issue_node_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_some() {
            return Err(StateError::DuplicateTask(
                repo_id.into(),
                issue_node_id.into(),
            ));
        }
        tx.execute("INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision) VALUES(?1,?2,?3,?4,?5,'preparing',?6)", params![id, repo_id, issue_node_id, candidate.repository, candidate.issue_number, config_revision])?;
        let evidence = SelectionEvidence {
            candidate: candidate.clone(),
            config_revision: config_revision.to_owned(),
            effective_config: EffectiveConfigSnapshot::from(config),
        };
        let payload = serde_json::to_string(&evidence)?;
        tx.execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,NULL,'selection',?2)",
            params![id, payload],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Nonterminal attempts must each be inspected on startup, including attempts
    /// without a launch intent (an inconsistent durable state is not safe to skip).
    pub fn pending_attempts(&self) -> Result<Vec<(String, String)>, StateError> {
        let mut statement = self.connection.prepare(
            "SELECT a.task_id,a.id FROM attempts a WHERE NOT (
             a.lifecycle='telemetry_lost' AND a.outcome IS NULL
             AND EXISTS (SELECT 1 FROM tasks t WHERE t.id=a.task_id AND t.state='pr_complete')
             AND EXISTS (SELECT 1 FROM reservations r WHERE r.task_id=a.task_id AND r.attempt_id=a.id AND r.status='released')
             AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind='telemetry_lost'
               AND CASE WHEN json_valid(e.payload) THEN json_type(e.payload,'$.actor')='text' AND length(trim(json_extract(e.payload,'$.actor')))>0
                 AND json_type(e.payload,'$.reason')='text' AND length(trim(json_extract(e.payload,'$.reason')))>0
                 AND json_type(e.payload,'$.observed_at_unix_secs')='integer' AND json_extract(e.payload,'$.observed_at_unix_secs')>0
                 AND json_type(e.payload,'$.os_ids')='array' AND json_array_length(e.payload,'$.os_ids')>0
                 AND json_type(e.payload,'$.exit_code') IS NULL AND json_type(e.payload,'$.signal') IS NULL ELSE 0 END)=1
             AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind='exit_pr_lookup'
               AND CASE WHEN json_valid(e.payload) THEN json_extract(e.payload,'$.status.status')='open' ELSE 0 END)=1
             AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind='verified_open_pr'
               AND CASE WHEN json_valid(e.payload) THEN json_type(e.payload,'$.id')='integer' AND json_extract(e.payload,'$.id')>0 ELSE 0 END)=1
             AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind IN ('attempt_exit','telemetry_lost','exit_pr_lookup','verified_open_pr'))=3
             AND a.id=(SELECT id FROM attempts WHERE task_id=a.task_id ORDER BY rowid DESC LIMIT 1)
             AND NOT EXISTS (SELECT 1 FROM attempts a2 WHERE a2.task_id=a.task_id AND a2.id<>a.id AND (a2.lifecycle!='completed' OR a2.outcome IS NULL OR EXISTS (SELECT 1 FROM reservations r2 WHERE r2.task_id=a2.task_id AND r2.attempt_id=a2.id AND r2.status='reserved') OR (EXISTS (SELECT 1 FROM evidence e2 WHERE e2.task_id=a2.task_id AND e2.attempt_id=a2.id AND e2.kind='attempt_exit') AND NOT EXISTS (SELECT 1 FROM evidence e2 WHERE e2.task_id=a2.task_id AND e2.attempt_id=a2.id AND e2.kind=CASE WHEN (SELECT COUNT(*) FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')=1 AND json_valid((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')) AND json_type((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')='array' AND json_array_length((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')>0 THEN 'pause_pr_lookup' WHEN (SELECT COUNT(*) FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')=1 AND json_valid((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')) AND json_type((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')='array' AND json_array_length((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')=0 THEN 'exit_pr_lookup' END AND CASE WHEN json_valid(e2.payload) THEN json_extract(e2.payload,'$.status.status')='absent' ELSE 0 END))))
           ) AND (a.lifecycle!='completed' OR a.outcome IS NULL OR EXISTS
             (SELECT 1 FROM reservations r WHERE r.attempt_id=a.id AND r.status='reserved') OR
             (EXISTS (SELECT 1 FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind='attempt_exit')
              AND NOT EXISTS (SELECT 1 FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind=CASE WHEN EXISTS
                 (SELECT 1 FROM intents i WHERE i.task_id=a.task_id AND i.attempt_id=a.id AND i.kind='stop') THEN 'pause_pr_lookup' ELSE 'exit_pr_lookup' END)))
             ORDER BY a.rowid",
        )?;
        Ok(statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?)
    }

    /// A live worker may be admitted alongside another only while its exact
    /// attempt still owns a reservation and has no recorded exit.
    pub(crate) fn active_attempt_reservation(
        &self,
        task_id: &str,
        attempt_id: &str,
    ) -> Result<bool, StateError> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM attempts a JOIN reservations r ON r.attempt_id=a.id
             JOIN tasks t ON t.id=a.task_id WHERE a.id=?1 AND a.task_id=?2
             AND a.lifecycle='launch_intended' AND a.outcome IS NULL
             AND r.task_id=?2 AND r.status='reserved' AND t.state='held'
             AND NOT EXISTS (SELECT 1 FROM evidence e WHERE e.attempt_id=a.id AND e.kind='attempt_exit')",
            params![attempt_id, task_id], |row| row.get::<_, i64>(0)
        )? == 1)
    }

    /// Persisted source operations without their proof block new selections.
    /// Also include preparing tasks where the process died before the first intent.
    pub fn unresolved_sources(&self) -> Result<Vec<(String, String)>, StateError> {
        let mut statement = self.connection.prepare(
            "SELECT t.id, 'selection' FROM tasks t WHERE t.state='preparing'
             UNION ALL
             SELECT t.id, 'prelaunch' FROM tasks t WHERE t.state NOT IN ('preparing','completed')
               AND NOT EXISTS (SELECT 1 FROM attempts a WHERE a.task_id=t.id)
             UNION ALL
             SELECT i.task_id, i.kind FROM intents i JOIN tasks t ON t.id=i.task_id
             WHERE t.state!='completed' AND (
               (i.kind='claim_assignment' AND NOT EXISTS
                 (SELECT 1 FROM evidence e WHERE e.task_id=i.task_id AND e.kind='claim_verified'))
               OR (i.kind='worktree_create' AND NOT EXISTS
                 (SELECT 1 FROM evidence e WHERE e.task_id=i.task_id AND e.kind='worktree_created'))
               OR (i.kind='stop' AND NOT EXISTS
                 (SELECT 1 FROM evidence e WHERE e.task_id=i.task_id AND e.attempt_id=i.attempt_id AND e.kind='attempt_exit')
                 AND NOT (
                   t.state='pr_complete'
                   AND EXISTS (SELECT 1 FROM attempts a JOIN reservations r ON r.task_id=a.task_id AND r.attempt_id=a.id
                     WHERE a.task_id=i.task_id AND a.id=i.attempt_id AND a.lifecycle='telemetry_lost'
                       AND a.outcome IS NULL AND r.status='released'
                       AND a.id=(SELECT id FROM attempts WHERE task_id=i.task_id ORDER BY rowid DESC LIMIT 1))
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=i.task_id AND e.attempt_id=i.attempt_id
                     AND e.kind IN ('telemetry_lost','exit_pr_lookup','verified_open_pr'))=3
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=i.task_id AND e.attempt_id=i.attempt_id
                     AND e.kind='telemetry_lost' AND CASE WHEN json_valid(e.payload) THEN
                       json_type(e.payload,'$.actor')='text' AND length(trim(json_extract(e.payload,'$.actor')))>0
                       AND json_type(e.payload,'$.reason')='text' AND length(trim(json_extract(e.payload,'$.reason')))>0
                       AND json_type(e.payload,'$.observed_at_unix_secs')='integer' AND json_extract(e.payload,'$.observed_at_unix_secs')>0
                       AND json_type(e.payload,'$.os_ids')='array' AND json_array_length(e.payload,'$.os_ids')>0
                       AND json_type(e.payload,'$.exit_code') IS NULL AND json_type(e.payload,'$.signal') IS NULL ELSE 0 END)=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=i.task_id AND e.attempt_id=i.attempt_id
                     AND e.kind='exit_pr_lookup' AND CASE WHEN json_valid(e.payload) THEN
                       json_extract(e.payload,'$.status.status')='open'
                       AND json_type(e.payload,'$.observed_at_unix_secs')='integer'
                       AND json_extract(e.payload,'$.observed_at_unix_secs')>0 ELSE 0 END)=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=i.task_id AND e.attempt_id=i.attempt_id
                     AND e.kind='verified_open_pr' AND CASE WHEN json_valid(e.payload) THEN
                       json_type(e.payload,'$.id')='integer' AND json_extract(e.payload,'$.id')>0 ELSE 0 END)=1
                 ))
             ) ORDER BY 1,2",
        )?;
        Ok(statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?)
    }

    pub fn existing_issue(&self, repo_id: &str, issue_id: &str) -> Result<bool, StateError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE tracker_repo_id=?1 AND issue_node_id=?2)",
            params![repo_id, issue_id],
            |row| row.get(0),
        )?)
    }

    /// Looks up persisted selection only; this does not establish eligibility or authorize dispatch.
    pub fn existing_target(&self, repository: &str, issue_number: u64) -> Result<bool, StateError> {
        if repository.trim().is_empty() || issue_number == 0 {
            return Err(StateError::InvalidTarget);
        }
        let mut statement = self
            .connection
            .prepare("SELECT COUNT(*) FROM tasks WHERE repository=?1 AND issue_number=?2")?;
        let count: i64 =
            statement.query_row(params![repository, issue_number], |row| row.get(0))?;
        match count {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(StateError::AmbiguousTarget(
                repository.to_owned(),
                issue_number,
            )),
        }
    }

    /// A selected task occupies a scheduling slot even if it is held before launch.
    /// A verified exit releases its reservation but leaves the task held for PR proof.
    pub fn ensure_dispatch_capacity(&self) -> Result<(), StateError> {
        let capacity: usize = self.connection.query_row(
            "SELECT value FROM state_meta WHERE key='capacity'",
            [],
            |row| row.get(0),
        )?;
        let active_reservations: usize = self.connection.query_row(
            "SELECT COUNT(DISTINCT task_id) FROM reservations WHERE status='reserved'",
            [],
            |row| row.get(0),
        )?;
        let unresolved_tasks: usize = self.connection.query_row(
            "SELECT COUNT(DISTINCT t.id) FROM tasks t WHERE t.state!='completed'
             AND NOT (t.state='pr_complete'
               AND NOT EXISTS (SELECT 1 FROM reservations r WHERE r.task_id=t.id AND r.status='reserved')
               AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='verified_open_pr'
                 AND CASE WHEN json_valid(e.payload) THEN json_type(e.payload,'$.id')='integer' AND json_extract(e.payload,'$.id') > 0 ELSE 0 END)=1
               AND (EXISTS (SELECT 1 FROM evidence p JOIN attempts a ON a.task_id=p.task_id AND a.id=p.attempt_id
                 JOIN reservations r ON r.task_id=a.task_id AND r.attempt_id=a.id
                 WHERE p.task_id=t.id AND p.kind='verified_open_pr'
                   AND CASE WHEN json_valid(p.payload) THEN json_type(p.payload,'$.id')='integer' AND json_extract(p.payload,'$.id') > 0 ELSE 0 END
                   AND a.id=(SELECT id FROM attempts WHERE task_id=t.id ORDER BY rowid DESC LIMIT 1)
                   AND a.lifecycle='completed' AND a.outcome IS NOT NULL AND r.status='released'
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.attempt_id=a.id AND e.kind='attempt_exit')=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='claim_verified')=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='worktree_created')=1
                   AND (SELECT COUNT(*) FROM attempts a2 WHERE a2.task_id=t.id AND (a2.lifecycle!='completed' OR a2.outcome IS NULL))=0)
               OR EXISTS (SELECT 1 FROM evidence p JOIN attempts a ON a.task_id=p.task_id AND a.id=p.attempt_id
                 JOIN reservations r ON r.task_id=a.task_id AND r.attempt_id=a.id
                 WHERE p.task_id=t.id AND p.kind='verified_open_pr'
                   AND CASE WHEN json_valid(p.payload) THEN json_type(p.payload,'$.id')='integer' AND json_extract(p.payload,'$.id')>0 ELSE 0 END
                   AND a.id=(SELECT id FROM attempts WHERE task_id=t.id ORDER BY rowid DESC LIMIT 1)
                   AND a.lifecycle='telemetry_lost' AND a.outcome IS NULL AND r.status='released'
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.attempt_id=a.id AND e.kind='telemetry_lost'
                     AND CASE WHEN json_valid(e.payload) THEN json_type(e.payload,'$.actor')='text' AND length(trim(json_extract(e.payload,'$.actor')))>0
                       AND json_type(e.payload,'$.reason')='text' AND length(trim(json_extract(e.payload,'$.reason')))>0
                       AND json_type(e.payload,'$.observed_at_unix_secs')='integer' AND json_extract(e.payload,'$.observed_at_unix_secs')>0
                       AND json_type(e.payload,'$.os_ids')='array' AND json_array_length(e.payload,'$.os_ids')>0
                       AND json_type(e.payload,'$.exit_code') IS NULL AND json_type(e.payload,'$.signal') IS NULL ELSE 0 END)=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.attempt_id=a.id AND e.kind='exit_pr_lookup'
                     AND CASE WHEN json_valid(e.payload) THEN json_extract(e.payload,'$.status.status')='open' ELSE 0 END)=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.attempt_id=a.id AND e.kind IN ('attempt_exit','telemetry_lost','exit_pr_lookup','verified_open_pr'))=3
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.attempt_id=a.id AND e.kind='verified_open_pr'
                     AND CASE WHEN json_valid(e.payload) THEN json_type(e.payload,'$.id')='integer' AND json_extract(e.payload,'$.id')>0 ELSE 0 END)=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='claim_verified')=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='worktree_created')=1
                   AND NOT EXISTS (SELECT 1 FROM attempts a2 WHERE a2.task_id=t.id AND a2.id<>a.id AND (a2.lifecycle!='completed' OR a2.outcome IS NULL OR EXISTS (SELECT 1 FROM reservations r2 WHERE r2.task_id=a2.task_id AND r2.attempt_id=a2.id AND r2.status='reserved') OR (EXISTS (SELECT 1 FROM evidence e2 WHERE e2.task_id=a2.task_id AND e2.attempt_id=a2.id AND e2.kind='attempt_exit') AND NOT EXISTS (SELECT 1 FROM evidence e2 WHERE e2.task_id=a2.task_id AND e2.attempt_id=a2.id AND e2.kind=CASE WHEN (SELECT COUNT(*) FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')=1 AND json_valid((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')) AND json_type((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')='array' AND json_array_length((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')>0 THEN 'pause_pr_lookup' WHEN (SELECT COUNT(*) FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')=1 AND json_valid((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')) AND json_type((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')='array' AND json_array_length((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')=0 THEN 'exit_pr_lookup' END AND CASE WHEN json_valid(e2.payload) THEN json_extract(e2.payload,'$.status.status')='absent' ELSE 0 END)))))))
             AND NOT (t.state='paused'
               AND EXISTS (SELECT 1 FROM evidence e WHERE e.task_id=t.id AND e.attempt_id=(SELECT id FROM attempts WHERE task_id=t.id ORDER BY rowid DESC LIMIT 1) AND e.kind='pause_pr_lookup' AND CASE WHEN json_valid(e.payload) THEN json_extract(e.payload,'$.status.status')='absent' ELSE 0 END)
               AND NOT EXISTS (SELECT 1 FROM reservations r WHERE r.task_id=t.id AND r.status='reserved')
               AND EXISTS (SELECT 1 FROM attempts a JOIN reservations r ON r.attempt_id=a.id
                 WHERE a.task_id=t.id AND a.lifecycle='completed' AND a.outcome IS NOT NULL
                   AND r.task_id=t.id AND r.status='released'
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.attempt_id=a.id AND e.kind='attempt_exit')=1
                   AND (SELECT COUNT(*) FROM intents i WHERE i.task_id=t.id AND i.attempt_id=a.id AND i.kind='stop')=1
                   AND (SELECT COUNT(*) FROM intents i WHERE i.task_id=t.id AND i.attempt_id=a.id AND i.kind='launch')=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='claim_verified')=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='worktree_created')=1
                   AND (SELECT COUNT(*) FROM attempts a2 WHERE a2.task_id=t.id AND (a2.lifecycle!='completed' OR a2.outcome IS NULL))=0)
             OR (t.state='attention'
               AND NOT EXISTS (SELECT 1 FROM reservations r WHERE r.task_id=t.id AND r.status='reserved')
               AND EXISTS (SELECT 1 FROM attempts a JOIN reservations r ON r.attempt_id=a.id AND r.task_id=t.id
                 JOIN evidence e ON e.task_id=t.id AND e.attempt_id=a.id AND e.kind='attempt_exit'
                 JOIN evidence p ON p.task_id=t.id AND p.attempt_id=a.id AND p.kind='exit_pr_lookup'
                 WHERE a.task_id=t.id AND a.id=(SELECT id FROM attempts WHERE task_id=t.id ORDER BY rowid DESC LIMIT 1)
                   AND a.lifecycle='completed' AND a.outcome IS NOT NULL AND r.status='released'
                   AND CASE WHEN json_valid(p.payload) THEN json_extract(p.payload,'$.status.status')='absent' ELSE 0 END
                   AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=a.id AND kind='attempt_exit')=1
                   AND p.rowid=(SELECT MAX(p2.rowid) FROM evidence p2 WHERE p2.task_id=t.id AND p2.attempt_id=a.id AND p2.kind='exit_pr_lookup')
                   AND CASE WHEN json_valid(e.payload) THEN json_array_length(e.payload,'$.stop_signals')=0 ELSE 0 END
                   AND (SELECT COUNT(*) FROM intents WHERE task_id=t.id AND attempt_id=a.id AND kind='launch')=1
                   AND (SELECT COUNT(*) FROM evidence WHERE task_id=t.id AND kind='claim_verified')=1
                   AND (SELECT COUNT(*) FROM evidence WHERE task_id=t.id AND kind='worktree_created')=1
                   AND (SELECT COUNT(*) FROM attempts WHERE task_id=t.id AND (lifecycle!='completed' OR outcome IS NULL))=0)))",
            [],
            |row| row.get(0),
        )?;
        if active_reservations >= capacity || unresolved_tasks >= capacity {
            return Err(StateError::Capacity {
                reserved: unresolved_tasks.max(active_reservations),
                capacity,
            });
        }
        Ok(())
    }

    /// Record a non-retriable failure without releasing any launch reservation.
    pub fn hold_task(&mut self, task_id: &str, reason: &str) -> Result<(), StateError> {
        let tx = self.connection.transaction()?;
        let changed = tx.execute("UPDATE tasks SET state='held' WHERE id=?1", [task_id])?;
        if changed != 1 {
            return Err(StateError::InvalidSelection);
        }
        tx.execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,NULL,'held_reason',?2)",
            params![task_id, reason],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn has_attempt(&self, task_id: &str, attempt_id: &str) -> Result<bool, StateError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM attempts WHERE task_id=?1 AND id=?2)",
                rusqlite::params![task_id, attempt_id],
                |row| row.get(0),
            )
            .map_err(StateError::from)
    }

    pub fn latest_attempt(&self, task_id: &str) -> Result<Option<String>, StateError> {
        self.connection
            .query_row(
                "SELECT id FROM attempts WHERE task_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1",
                [task_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(StateError::from)
    }

    pub fn task_phase(&self, task_id: &str) -> Result<Option<String>, StateError> {
        self.connection
            .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(StateError::from)
    }

    pub fn held_reason(&self, task_id: &str) -> Result<Option<String>, StateError> {
        self.connection.query_row(
            "SELECT payload FROM evidence WHERE task_id=?1 AND kind='held_reason' ORDER BY sequence DESC LIMIT 1",
            [task_id], |row| row.get(0),
        ).optional().map_err(StateError::from)
    }

    pub fn claim_assignment_login(&self, task_id: &str) -> Result<Option<String>, StateError> {
        let evidence = self.selection_evidence(task_id)?;
        Ok(evidence.map(|evidence| evidence.effective_config.assignment_login))
    }

    pub fn source_claim_intent(&self, task_id: &str) -> Result<Option<String>, StateError> {
        proofs::unique_payload(&self.connection, false, task_id, None, "claim_assignment")
    }

    pub fn record_claim_intent(
        &mut self,
        task_id: &str,
        principal: &str,
        repository: &str,
        number: u64,
    ) -> Result<(), StateError> {
        let tx = self.connection.transaction()?;
        let prior: i64 = tx.query_row(
            "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND kind='claim_assignment'",
            [task_id],
            |r| r.get(0),
        )?;
        if prior != 0 {
            return Err(StateError::InvalidSelection);
        }
        tx.execute(
            "INSERT INTO intents(id,task_id,kind,detail) VALUES(?1,?2,'claim_assignment',?3)",
            params![
                format!("claim-{task_id}"),
                task_id,
                serde_json::json!({"principal":principal,"repository":repository,"number":number})
                    .to_string()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_task_phase(&mut self, task_id: &str, phase: &str) -> Result<(), StateError> {
        let changed = self.connection.execute(
            "UPDATE tasks SET state=?2 WHERE id=?1",
            params![task_id, phase],
        )?;
        if changed != 1 {
            return Err(StateError::InvalidSelection);
        }
        Ok(())
    }

    pub fn record_intent(
        &mut self,
        id: &str,
        task_id: &str,
        attempt_id: Option<&str>,
        kind: &str,
        detail: &str,
    ) -> Result<(), StateError> {
        self.connection.execute(
            "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,?4,?5)",
            params![id, task_id, attempt_id, kind, detail],
        )?;
        Ok(())
    }

    pub fn record_evidence(
        &mut self,
        task_id: &str,
        attempt_id: Option<&str>,
        kind: &str,
        payload: &str,
    ) -> Result<i64, StateError> {
        self.connection.execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,?3,?4)",
            params![task_id, attempt_id, kind, payload],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn record_verified_open_pr(
        &mut self,
        task_id: &str,
        attempt_id: &str,
        proof: &VerifiedOpenPr,
    ) -> Result<(), StateError> {
        if !(self.stopped_exit_for_pause(task_id, attempt_id)?
            || self.natural_exit_for_attention(task_id, attempt_id)?)
            || proof.id == 0
            || proof.attempt_id != attempt_id
            || proof.observed_at == 0
        {
            return Err(StateError::LaunchBlocked);
        }
        let tx = self.connection.transaction()?;
        amended_dispatch::verify_committed_amendment(&tx, task_id, attempt_id)?;
        validate_verified_open_pr(&tx, task_id, attempt_id, proof)?;
        tx.execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'verified_open_pr',?3)",
            params![task_id, attempt_id, serde_json::to_string(proof)?],
        )?;
        tx.execute(
            "UPDATE tasks SET state='pr_complete' WHERE id=?1 AND state='held'",
            [task_id],
        )?;
        if tx.changes() != 1 {
            return Err(StateError::LaunchBlocked);
        }
        amended_completion::record(&tx, task_id, attempt_id)?;
        amended_dispatch::verify_committed_amendment(&tx, task_id, attempt_id)?;
        tx.commit()?;
        Ok(())
    }
    /// A reserved attempt remains reserved even if the supervisor is unreachable.
    /// Repeated requests reuse the first durable intent rather than adding a new one.
    pub fn record_stop_intent(
        &mut self,
        task_id: &str,
        attempt_id: &str,
    ) -> Result<(), StateError> {
        let tx = self.connection.transaction()?;
        let active: i64 = tx.query_row(
            "SELECT COUNT(*) FROM attempts a JOIN reservations r ON r.attempt_id=a.id
             JOIN tasks t ON t.id=a.task_id
             WHERE a.id=?1 AND a.task_id=?2 AND a.lifecycle='launch_intended'
               AND a.outcome IS NULL AND r.task_id=?2 AND r.status='reserved' AND t.state='held'
               AND (SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND task_id=?2 AND kind='launch')=1",
            params![attempt_id, task_id], |row| row.get(0)
        )?;
        if active != 1 {
            return Err(StateError::LaunchBlocked);
        }
        let prior: Vec<String> = tx
            .prepare("SELECT detail FROM intents WHERE attempt_id=?1 AND kind='stop'")?
            .query_map([attempt_id], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        let detail = serde_json::json!({"task_id":task_id,"attempt_id":attempt_id}).to_string();
        if prior.is_empty() {
            tx.execute(
                "INSERT INTO intents(id,task_id,attempt_id,kind,detail)
                 VALUES(?1,?2,?3,'stop',?4)",
                params![format!("stop-{attempt_id}"), task_id, attempt_id, detail],
            )?;
        } else if prior != [detail] {
            return Err(StateError::LaunchBlocked);
        }
        tx.commit()?;
        Ok(())
    }

    pub fn stop_intent(
        &self,
        task_id: &str,
        attempt_id: &str,
    ) -> Result<Option<String>, StateError> {
        self.intent_payload(task_id, attempt_id, "stop")
    }

    pub fn reserve(&mut self, task_id: &str, attempt_id: &str) -> Result<(), StateError> {
        let tx = self.connection.transaction()?;
        let capacity: usize = tx.query_row(
            "SELECT value FROM state_meta WHERE key='capacity'",
            [],
            |r| r.get(0),
        )?;
        let reserved: usize = tx.query_row(
            "SELECT COUNT(*) FROM reservations WHERE status='reserved'",
            [],
            |r| r.get(0),
        )?;
        if reserved >= capacity {
            return Err(StateError::Capacity { reserved, capacity });
        }
        tx.execute(
            "INSERT INTO reservations(attempt_id,task_id,status) VALUES(?2,?1,'reserved')",
            params![task_id, attempt_id],
        )?;
        tx.execute(
            "INSERT INTO attempts(id,task_id,lifecycle) VALUES(?1,?2,'launch_intended')",
            params![attempt_id, task_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Returns the first and latest launch plans and the verified latest exit only
    /// when a paused task has no outstanding worker or reservation.
    pub fn resume_context(&self, task_id: &str) -> Result<(String, String, String), StateError> {
        resume_context(&self.connection, task_id)
    }

    /// Persists a plan without granting permission to start a process.
    pub fn hold_launch_intent(
        &mut self,
        task_id: &str,
        attempt_id: &str,
        detail: &str,
    ) -> Result<(), StateError> {
        hold_launch(self, task_id, attempt_id, detail, None)
    }

    pub fn launch_intent(&self, attempt_id: &str) -> Result<Option<String>, StateError> {
        self.connection
            .query_row(
                "SELECT detail FROM intents WHERE attempt_id=?1 AND kind='launch'",
                [attempt_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(StateError::from)
    }

    /// A dispatch marker is written once, before spawning. An uncertain spawn
    /// cannot be retried automatically, even if the supervisor never became ready.
    pub fn begin_supervision(
        &mut self,
        task_id: &str,
        attempt_id: &str,
        plan: &str,
    ) -> Result<(), StateError> {
        let tx = self.connection.transaction()?;
        let persisted: Option<String> = tx
            .query_row(
                "SELECT i.detail FROM intents i JOIN attempts a ON a.id=i.attempt_id
             JOIN reservations r ON r.attempt_id=a.id
             JOIN tasks t ON t.id=a.task_id
             WHERE i.kind='launch' AND i.attempt_id=?1 AND i.task_id=?2
               AND r.task_id=?2 AND r.status='reserved' AND t.state='held'
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND kind='claim_verified')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND kind='worktree_created')=1",
                params![attempt_id, task_id],
                |row| row.get(0),
            )
            .optional()?;
        let amendments: i64 = tx.query_row(
            "SELECT (SELECT COUNT(*) FROM evidence WHERE (task_id=?1 OR attempt_id=?2) AND kind='initial_branch_removed')
             + (SELECT COUNT(*) FROM intents WHERE (task_id=?1 OR attempt_id=?2) AND kind='initial_branch_removal_seal')",
            params![task_id, attempt_id], |row| row.get(0),
        )?;
        if persisted.as_deref() != Some(plan) || amendments != 0 {
            return Err(StateError::LaunchBlocked);
        }
        let previous: i64 = tx.query_row(
            "SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND kind='supervisor_dispatch'",
            [attempt_id],
            |row| row.get(0),
        )?;
        if previous != 0 {
            return Err(StateError::LaunchBlocked);
        }
        tx.execute("INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'supervisor_dispatch',?4)",
            params![format!("supervisor-{attempt_id}"), task_id, attempt_id, plan])?;
        tx.commit()?;
        Ok(())
    }

    pub fn release_reservation(&mut self, attempt_id: &str) -> Result<usize, StateError> {
        let launch: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND kind='launch'",
            [attempt_id],
            |row| row.get(0),
        )?;
        if launch != 0 {
            return Err(StateError::LaunchBlocked);
        }
        self.connection.execute(
            "UPDATE reservations SET status='released' WHERE attempt_id=?1 AND status='reserved'",
            [attempt_id],
        )?;
        Ok(self.connection.changes() as usize)
    }

    pub(crate) fn reconciled_exit(
        &self,
        task_id: &str,
        attempt_id: &str,
        evidence: &str,
        outcome: &str,
    ) -> Result<bool, StateError> {
        let row: Option<(String, Option<String>, String)> = self
            .connection
            .query_row(
                "SELECT a.lifecycle,a.outcome,r.status FROM attempts a
             JOIN reservations r ON r.attempt_id=a.id
             WHERE a.id=?1 AND a.task_id=?2 AND r.task_id=?2",
                params![attempt_id, task_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((lifecycle, persisted_outcome, reservation)) = row else {
            return Err(StateError::LaunchBlocked);
        };
        let exits: Vec<(String, String)> = self
            .connection
            .prepare(
                "SELECT task_id,payload FROM evidence WHERE attempt_id=?1 AND kind='attempt_exit'",
            )?
            .query_map([attempt_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        if lifecycle == "launch_intended"
            && persisted_outcome.is_none()
            && reservation == "reserved"
            && exits.is_empty()
        {
            return Ok(false);
        }
        if lifecycle == "completed"
            && persisted_outcome.as_deref() == Some(outcome)
            && reservation == "released"
            && exits == [(task_id.to_owned(), evidence.to_owned())]
        {
            return Ok(true);
        }
        Err(StateError::LaunchBlocked)
    }

    /// Only a completed, independently reconciled stop can request PR proof.
    pub fn stopped_exit_for_pause(
        &self,
        task_id: &str,
        attempt_id: &str,
    ) -> Result<bool, StateError> {
        let exit: Option<String> = self.connection.query_row(
            "SELECT e.payload FROM evidence e JOIN attempts a ON a.id=e.attempt_id AND a.task_id=e.task_id
             JOIN reservations r ON r.attempt_id=a.id AND r.task_id=a.task_id
             JOIN tasks t ON t.id=a.task_id
             WHERE e.task_id=?1 AND e.attempt_id=?2 AND e.kind='attempt_exit'
               AND t.state='held' AND a.lifecycle='completed' AND a.outcome IS NOT NULL AND r.status='released'
               AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='stop')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='attempt_exit')=1
               AND (SELECT COUNT(*) FROM reservations WHERE task_id=?1 AND status='reserved')=0
               AND (SELECT COUNT(*) FROM attempts WHERE task_id=?1 AND (lifecycle!='completed' OR outcome IS NULL))=0
               AND a.id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1)",
            params![task_id, attempt_id], |row| row.get(0)
        ).optional()?;
        let Some(exit) = exit else {
            return Ok(false);
        };
        let receipt: crate::model::ExitReceipt = serde_json::from_str(&exit)?;
        Ok(receipt.attempt_id == attempt_id && !receipt.stop_signals.is_empty())
    }

    /// Only a terminal, released, naturally exited latest attempt may enter attention.
    pub fn natural_exit_for_attention(
        &self,
        task_id: &str,
        attempt_id: &str,
    ) -> Result<bool, StateError> {
        let exit: Option<(String, String)> = self.connection.query_row(
            "SELECT e.payload,a.outcome FROM evidence e
             JOIN attempts a ON a.id=e.attempt_id AND a.task_id=e.task_id
             JOIN reservations r ON r.attempt_id=a.id AND r.task_id=a.task_id
             JOIN tasks t ON t.id=a.task_id
             WHERE e.task_id=?1 AND e.attempt_id=?2 AND e.kind='attempt_exit'
               AND t.state='held' AND a.lifecycle='completed' AND a.outcome IS NOT NULL AND r.status='released'
               AND a.id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1)
               AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=?2 AND kind='attempt_exit')=1
               AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='launch')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='claim_verified')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='worktree_created')=1
               AND (SELECT COUNT(*) FROM reservations WHERE task_id=?1 AND status='reserved')=0
               AND (SELECT COUNT(*) FROM attempts WHERE task_id=?1 AND (lifecycle!='completed' OR outcome IS NULL))=0",
            params![task_id, attempt_id], |row| Ok((row.get(0)?, row.get(1)?))
        ).optional()?;
        let Some((exit, outcome)) = exit else {
            return Ok(false);
        };
        let receipt: crate::model::ExitReceipt = serde_json::from_str(&exit)?;
        Ok(receipt.attempt_id == attempt_id
            && receipt.stop_signals.is_empty()
            && outcome
                == format!(
                    "exit_code={:?};signal={:?}",
                    receipt.exit_code, receipt.signal
                ))
    }

    /// An absent PR observation and the attention transition commit together.
    pub fn record_exit_pr_lookup(
        &mut self,
        task_id: &str,
        attempt_id: &str,
        proof: &ExitPrEvidence,
    ) -> Result<(), StateError> {
        if !self.natural_exit_for_attention(task_id, attempt_id)?
            || proof.observed_at_unix_secs == 0
        {
            return Err(StateError::LaunchBlocked);
        }
        let tx = self.connection.transaction()?;
        let prior: Vec<String> = tx
            .prepare(
                "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='exit_pr_lookup' ORDER BY sequence",
            )?
            .query_map(params![task_id, attempt_id], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        if prior.iter().any(|payload| {
            !matches!(
                serde_json::from_str::<ExitPrEvidence>(payload).map(|evidence| evidence.status),
                Ok(PausePrStatus::Error { .. } | PausePrStatus::Ambiguous | PausePrStatus::Open)
            )
        }) {
            return Err(StateError::LaunchBlocked);
        }
        let selection: String = tx.query_row(
            "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection'",
            [task_id], |row| row.get(0)
        )?;
        let selection: SelectionEvidence = serde_json::from_str(&selection)?;
        if selection.candidate.mapping.code_repository != proof.repository {
            return Err(StateError::LaunchBlocked);
        }
        tx.execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'exit_pr_lookup',?3)",
            params![task_id, attempt_id, serde_json::to_string(proof)?]
        )?;
        match &proof.status {
            PausePrStatus::Absent => {
                tx.execute(
                    "UPDATE tasks SET state='attention' WHERE id=?1 AND state='held'",
                    [task_id],
                )?;
                if tx.changes() != 1 {
                    return Err(StateError::LaunchBlocked);
                }
                tx.execute(
                    "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'attention_reason','natural exit without open PR')",
                    params![task_id, attempt_id]
                )?;
            }
            status => {
                let reason = match status {
                    PausePrStatus::Open => "exit PR present",
                    PausePrStatus::Ambiguous => "exit PR ambiguous",
                    PausePrStatus::Error { .. } => "exit PR read failed",
                    PausePrStatus::Absent => unreachable!(),
                };
                tx.execute(
                    "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'held_reason',?3)",
                    params![task_id, attempt_id, reason]
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The fresh lookup and phase change are one transaction. A crash between
    /// lookup and commit leaves held work requiring another read.
    pub fn record_pause_pr_lookup(
        &mut self,
        task_id: &str,
        attempt_id: &str,
        proof: &PausePrEvidence,
    ) -> Result<(), StateError> {
        if !self.stopped_exit_for_pause(task_id, attempt_id)? || proof.observed_at_unix_secs == 0 {
            return Err(StateError::LaunchBlocked);
        }
        let tx = self.connection.transaction()?;
        let selection: String = tx.query_row(
            "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection'",
            [task_id], |row| row.get(0)
        )?;
        let selection: SelectionEvidence = serde_json::from_str(&selection)?;
        if selection.candidate.mapping.code_repository != proof.repository {
            return Err(StateError::LaunchBlocked);
        }
        tx.execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'pause_pr_lookup',?3)",
            params![task_id, attempt_id, serde_json::to_string(proof)?]
        )?;
        match &proof.status {
            PausePrStatus::Absent => {
                tx.execute(
                    "UPDATE tasks SET state='paused' WHERE id=?1 AND state='held'",
                    [task_id],
                )?;
                if tx.changes() != 1 {
                    return Err(StateError::LaunchBlocked);
                }
            }
            status => {
                let reason = match status {
                    PausePrStatus::Open => "pause PR present",
                    PausePrStatus::Ambiguous => "pause PR ambiguous",
                    PausePrStatus::Error { .. } => "pause PR read failed",
                    PausePrStatus::Absent => unreachable!(),
                };
                tx.execute(
                    "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'held_reason',?3)",
                    params![task_id, attempt_id, reason]
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn evidence_payload(
        &self,
        task_id: &str,
        attempt_id: Option<&str>,
        kind: &str,
    ) -> Result<Option<String>, StateError> {
        proofs::unique_payload(&self.connection, true, task_id, attempt_id, kind)
    }

    pub fn evidence_payloads(
        &self,
        task_id: &str,
        attempt_id: &str,
        kind: &str,
    ) -> Result<Vec<String>, StateError> {
        let mut statement = self.connection.prepare(
            "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind=?3 ORDER BY sequence",
        )?;
        let payloads = statement
            .query_map(params![task_id, attempt_id, kind], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(payloads)
    }

    pub(crate) fn intent_payload(
        &self,
        task_id: &str,
        attempt_id: &str,
        kind: &str,
    ) -> Result<Option<String>, StateError> {
        proofs::unique_payload(&self.connection, false, task_id, Some(attempt_id), kind)
    }

    pub fn reservation_count(&self) -> Result<usize, StateError> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM reservations WHERE status='reserved'",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn task_count(&self) -> Result<usize, StateError> {
        Ok(self
            .connection
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?)
    }

    pub fn evidence_kinds(&self, task_id: &str) -> Result<Vec<String>, StateError> {
        let mut statement = self
            .connection
            .prepare("SELECT kind FROM evidence WHERE task_id=?1 ORDER BY sequence")?;
        let rows = statement.query_map([task_id], |r| r.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn selection_evidence(
        &self,
        task_id: &str,
    ) -> Result<Option<SelectionEvidence>, StateError> {
        let payload: Option<String> = self.connection.query_row(
            "SELECT payload FROM evidence WHERE task_id=?1 AND kind='selection' ORDER BY sequence LIMIT 1",
            [task_id], |row| row.get(0),
        ).optional()?;
        payload
            .map(|payload| serde_json::from_str(&payload).map_err(StateError::from))
            .transpose()
    }
}

#[cfg(test)]
mod recovery_tests;

pub(crate) fn initial_launch(store: &StateStore, task_id: &str) -> Result<String, StateError> {
    let first: String = store.connection.query_row(
        "SELECT i.detail FROM attempts a JOIN intents i ON i.attempt_id=a.id
         WHERE a.task_id=?1 AND i.task_id=?1 AND i.kind='launch' ORDER BY a.rowid LIMIT 1",
        [task_id],
        |row| row.get(0),
    )?;
    Ok(first)
}

fn hold_launch(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    detail: &str,
    retry: Option<&RetryAuthorization>,
) -> Result<(), StateError> {
    if attempt_id.is_empty()
        || attempt_id.len() > 128
        || !attempt_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(StateError::LaunchBlocked);
    }
    let tx = store.connection.transaction()?;
    let phase: Option<String> = tx
        .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
            row.get(0)
        })
        .optional()?;
    authorize_launch_phase(&tx, task_id, attempt_id, phase.as_deref(), retry)?;
    let prior_intents: i64 = tx.query_row(
        "SELECT COUNT(*) FROM intents WHERE attempt_id=?1",
        [attempt_id],
        |row| row.get(0),
    )?;
    if prior_intents != 0 {
        return Err(StateError::LaunchBlocked);
    }
    let capacity: usize = tx.query_row(
        "SELECT value FROM state_meta WHERE key='capacity'",
        [],
        |row| row.get(0),
    )?;
    let reserved: usize = tx.query_row(
        "SELECT COUNT(*) FROM reservations WHERE status='reserved'",
        [],
        |row| row.get(0),
    )?;
    if reserved >= capacity {
        return Err(StateError::Capacity { reserved, capacity });
    }
    tx.execute(
        "INSERT INTO attempts(id,task_id,lifecycle) VALUES(?1,?2,'launch_intended')",
        params![attempt_id, task_id],
    )?;
    tx.execute(
        "INSERT INTO reservations(attempt_id,task_id,status) VALUES(?1,?2,'reserved')",
        params![attempt_id, task_id],
    )?;
    tx.execute(
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'launch',?4)",
        params![format!("launch-{attempt_id}"), task_id, attempt_id, detail],
    )?;
    if let Some(audit) = retry {
        tx.execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'retry_authorized',?3)",
            params![task_id, attempt_id, serde_json::to_string(audit)?],
        )?;
        tx.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'retry_pr_lookup',?3)", params![task_id, attempt_id, serde_json::to_string(&audit.pr)?])?;
    }
    tx.execute("UPDATE tasks SET state='held' WHERE id=?1", [task_id])?;
    tx.commit()?;
    Ok(())
}

pub fn retry_context_for_task(
    store: &StateStore,
    task_id: &str,
    previous_attempt_id: &str,
) -> Result<(crate::model::LaunchPlan, crate::model::ExitReceipt), StateError> {
    retry_context(&store.connection, task_id, previous_attempt_id)
}

pub(crate) fn selection_for_attempt(
    store: &StateStore,
    plan: &crate::model::LaunchPlan,
) -> Result<SelectionEvidence, StateError> {
    attempt_selection(&store.connection, plan)
}

pub(crate) fn hold_retry_intent(
    store: &mut StateStore,
    audit: &RetryAuthorization,
) -> Result<(), StateError> {
    hold_launch(
        store,
        &audit.plan.task_id,
        &audit.plan.attempt_id,
        &serde_json::to_string(&audit.plan)?,
        Some(audit),
    )
}

fn authorize_launch_phase(
    tx: &Transaction<'_>,
    task_id: &str,
    attempt_id: &str,
    phase: Option<&str>,
    retry: Option<&RetryAuthorization>,
) -> Result<(), StateError> {
    let claims: i64 = tx.query_row(
        "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='claim_verified'",
        [task_id],
        |row| row.get(0),
    )?;
    let worktrees: i64 = tx.query_row(
        "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='worktree_created'",
        [task_id],
        |row| row.get(0),
    )?;
    let attempts: i64 = tx.query_row(
        "SELECT COUNT(*) FROM attempts WHERE task_id=?1",
        [task_id],
        |row| row.get(0),
    )?;
    match phase {
        Some("claimed") if retry.is_none() && attempts == 0 && claims == 1 && worktrees == 1 => {}
        Some("paused") if retry.is_none() && claims == 1 && worktrees == 1 => {
            resume_context(tx, task_id)?;
        }
        Some("attention") if retry.is_some() && claims == 1 && worktrees == 1 => {
            authorize_retry(tx, retry.expect("retry checked above"), task_id, attempt_id)?;
        }
        _ => return Err(StateError::LaunchBlocked),
    }
    Ok(())
}

fn authorize_retry(
    tx: &Transaction<'_>,
    audit: &RetryAuthorization,
    task_id: &str,
    attempt_id: &str,
) -> Result<(), StateError> {
    let (prior, _) = retry_context(tx, task_id, &audit.previous_plan.attempt_id)?;
    let previous = attempt_selection(tx, &prior)?;
    if audit.previous_plan != prior
        || !retry_evidence_matches(tx, audit, &previous)?
        || audit.previous_config != previous.effective_config
        || !same_task_config(&previous.effective_config, &audit.config)
        || audit.actor != previous.candidate.mapping.allowed_pr_author
        || audit.reason.trim().is_empty()
        || audit.plan.attempt_id != attempt_id
        || audit.plan.task_id != task_id
        || audit.reservation != attempt_id
        || audit.pr.status != PausePrStatus::Absent
        || audit.pr.observed_at_unix_secs == 0
        || audit.pr.repository != previous.candidate.mapping.code_repository
        || audit.plan.config_revision.trim().is_empty()
        || audit.plan.config_revision == prior.config_revision
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

fn validate_verified_open_pr(
    tx: &Transaction<'_>,
    task_id: &str,
    attempt_id: &str,
    proof: &VerifiedOpenPr,
) -> Result<(), StateError> {
    proofs::validate_verified_open_pr(tx, task_id, attempt_id, proof)
}
