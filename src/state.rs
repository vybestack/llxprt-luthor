use crate::{
    config::{CommandTemplate, Config, Mapping, Source},
    eligibility::Candidate,
};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
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

    pub fn release_reservation(&mut self, attempt_id: &str) -> Result<usize, StateError> {
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
