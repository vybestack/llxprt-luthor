use crate::{
    config::{CommandTemplate, Config, Mapping, Source},
    eligibility::Candidate,
};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StateError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    #[error("task identity already exists: {0}/{1}")]
    DuplicateTask(String, String),
    #[error("capacity exhausted: {reserved} reservations for capacity {capacity}")]
    Capacity { reserved: usize, capacity: usize },
    #[error("capacity must be positive")]
    InvalidCapacity,
    #[error("invalid configuration")]
    InvalidConfig,
    #[error("candidate selection does not match effective configuration")]
    InvalidSelection,
    #[error("launch requires a verified worktree and an unused, accounted task")]
    LaunchBlocked,
    #[error("configured capacity {configured} does not match persisted capacity {persisted}")]
    CapacityMismatch { configured: usize, persisted: usize },
    #[error("unsupported database version {0}")]
    UnsupportedDatabaseVersion(i32),
}

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
        .map_or(candidate.milestone_title.is_none(), |title| {
            candidate.milestone_title.as_ref() == Some(title)
        });
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
    connection: Connection,
    _lock: StateLock,
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeIntent {
    pub path: PathBuf,
    pub branch: String,
    pub base: String,
    pub repository: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeIdentity {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub branch: String,
    pub base: String,
    pub head: String,
    pub repository: String,
    pub git_directory: PathBuf,
    pub remote: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub intent: WorktreeIntent,
    pub identity: Option<WorktreeIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct EffectiveConfigSnapshot {
    pub state_root: PathBuf,
    pub worktree_root: PathBuf,
    pub capacity: usize,
    pub assignment_login: String,
    pub sources: Vec<Source>,
    pub mappings: Vec<Mapping>,
    pub initial: CommandTemplate,
    pub resume: CommandTemplate,
}

impl From<&Config> for EffectiveConfigSnapshot {
    fn from(config: &Config) -> Self {
        Self {
            state_root: config.state_root.clone(),
            worktree_root: config.worktree_root.clone(),
            capacity: config.capacity,
            assignment_login: config.assignment_login.clone(),
            sources: config.sources.clone(),
            mappings: config.mappings.clone(),
            initial: config.initial.clone(),
            resume: config.resume.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct SelectionEvidence {
    pub candidate: Candidate,
    pub config_revision: String,
    pub effective_config: EffectiveConfigSnapshot,
}

impl StateStore {
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

    pub fn worktree_intent(&self, task_id: &str) -> Result<Option<WorktreeIntent>, StateError> {
        Ok(self.worktree_record(task_id)?.map(|record| record.intent))
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

    pub fn claim_assignment_login(&self, task_id: &str) -> Result<Option<String>, StateError> {
        let evidence = self.selection_evidence(task_id)?;
        Ok(evidence.map(|evidence| evidence.effective_config.assignment_login))
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

    /// Persists a plan without granting permission to start a process.
    pub fn hold_launch_intent(
        &mut self,
        task_id: &str,
        attempt_id: &str,
        detail: &str,
    ) -> Result<(), StateError> {
        if attempt_id.is_empty()
            || attempt_id.len() > 128
            || !attempt_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(StateError::LaunchBlocked);
        }
        let tx = self.connection.transaction()?;
        let phase: Option<String> = tx
            .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
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
        if phase.as_deref() != Some("claimed") || claims != 1 || worktrees != 1 || attempts != 0 {
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
        tx.execute("UPDATE tasks SET state='held' WHERE id=?1", [task_id])?;
        tx.commit()?;
        Ok(())
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
