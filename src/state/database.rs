use crate::model::StateError;
use fs2::FileExt;
use rusqlite::Connection;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

struct StateLock(File);

impl Drop for StateLock {
    fn drop(&mut self) {
        self.0.unlock().expect("failed to release state lock");
    }
}

pub struct StateStore {
    pub(crate) connection: Connection,
    _lock: StateLock,
    pub(crate) root: PathBuf,
}

impl StateStore {
    pub fn open(root: impl AsRef<Path>, capacity: usize) -> Result<Self, StateError> {
        open_store(root, capacity)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn open_store(root: impl AsRef<Path>, capacity: usize) -> Result<StateStore, StateError> {
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
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS state_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);",
        )?;
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
    Ok(StateStore {
        connection,
        _lock: StateLock(lock),
        root: root.to_path_buf(),
    })
}
