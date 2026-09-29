use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
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
    #[error("task identity already exists: {0}/{1}")]
    DuplicateTask(String, String),
    #[error("capacity exhausted: {reserved} reservations for capacity {capacity}")]
    Capacity { reserved: usize, capacity: usize },
    #[error("capacity must be positive")]
    InvalidCapacity,
    #[error("configured capacity {configured} does not match persisted capacity {persisted}")]
    CapacityMismatch { configured: usize, persisted: usize },
    #[error("unsupported database version {0}")]
    UnsupportedDatabaseVersion(i32),
}

pub struct StateStore {
    connection: Connection,
    _lock: File,
    root: PathBuf,
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
            .read(true)
            .write(true)
            .open(root.join("coordinator.lock"))?;
        lock.try_lock_exclusive().map_err(StateError::Io)?;
        let mut connection = Connection::open(root.join("state.sqlite3"))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let version: i32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 2 {
            return Err(StateError::UnsupportedDatabaseVersion(version));
        }
        let tx = connection.transaction()?;
        if version == 0 {
            tx.execute_batch("CREATE TABLE tasks (id TEXT PRIMARY KEY, tracker_repo_id TEXT NOT NULL, issue_node_id TEXT NOT NULL, repository TEXT NOT NULL, issue_number INTEGER NOT NULL, state TEXT NOT NULL, config_revision TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(tracker_repo_id, issue_node_id));
                CREATE TABLE attempts (id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), lifecycle TEXT NOT NULL, outcome TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                CREATE TABLE intents (sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                CREATE TABLE reservations (task_id TEXT PRIMARY KEY REFERENCES tasks(id), attempt_id TEXT NOT NULL UNIQUE, status TEXT NOT NULL CHECK(status IN ('reserved','released')), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
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
        tx.commit()?;
        Ok(Self {
            connection,
            _lock: lock,
            root: root.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn create_task(
        &mut self,
        id: &str,
        repo_id: &str,
        issue_node_id: &str,
        repository: &str,
        number: u64,
        revision: &str,
    ) -> Result<(), StateError> {
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
        tx.execute("INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision) VALUES(?1,?2,?3,?4,?5,'preparing',?6)", params![id, repo_id, issue_node_id, repository, number, revision])?;
        tx.commit()?;
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
            "INSERT INTO reservations(task_id,attempt_id,status) VALUES(?1,?2,'reserved')",
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
}
