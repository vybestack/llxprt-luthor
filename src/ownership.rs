//! A cooperative, process-lifetime lock for a task's worktree.
//! The state-root coordinator lock serializes decisions; this lock survives it
//! when an already-dispatched worker continues after the coordinator exits.
use fs2::FileExt;
use rusqlite::{Connection, params};
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};
use thiserror::Error;

pub const FD_ENV: &str = "LUTHOR_WORKTREE_OWNER_FD";

#[derive(Debug)]
pub enum OwnershipError {
    Busy,
    Unavailable,
}

#[derive(Debug, Error)]
pub enum OwnershipProtocolError {
    #[error("worktree ownership protocol conflicts with persisted state")]
    Conflict,
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorktreeOwnerProtocol {
    version: u8,
    task_id: String,
    attempt_id: String,
    dev: u64,
    ino: u64,
}

pub struct WorktreeOwner {
    file: File,
}

fn lock_path(root: &Path, task: &str) -> Result<PathBuf, OwnershipError> {
    if task.is_empty()
        || task.len() > 128
        || !task
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(OwnershipError::Unavailable);
    }
    let dir = fs::symlink_metadata(root).map_err(|_| OwnershipError::Unavailable)?;
    if !dir.file_type().is_dir()
        || dir.uid() != unsafe { libc::geteuid() }
        || dir.permissions().mode() & 0o777 != 0o700
    {
        return Err(OwnershipError::Unavailable);
    }
    Ok(root.join(format!("worktree-{task}.lock")))
}

fn verify_file(root: &Path, task: &str, file: &File) -> Result<(), OwnershipError> {
    let path = lock_path(root, task)?;
    let opened = file.metadata().map_err(|_| OwnershipError::Unavailable)?;
    let named = fs::symlink_metadata(path).map_err(|_| OwnershipError::Unavailable)?;
    if !opened.is_file()
        || !named.file_type().is_file()
        || opened.uid() != unsafe { libc::geteuid() }
        || opened.mode() & 0o777 != 0o600
        || opened.nlink() != 1
        || (opened.dev(), opened.ino()) != (named.dev(), named.ino())
    {
        return Err(OwnershipError::Unavailable);
    }
    Ok(())
}

pub trait WorktreeOwnerInternal {
    fn inherited(root: &Path, task: &str) -> Result<Self, OwnershipError>
    where
        Self: Sized;
    fn verify_protocol(
        &self,
        connection: &Connection,
        root: &Path,
        task: &str,
        attempt: &str,
    ) -> Result<(), OwnershipProtocolError>;
    fn verify_protocol_in_snapshot(
        &self,
        tx: &rusqlite::Transaction<'_>,
        root: &Path,
        task: &str,
        attempt: &str,
    ) -> Result<(), OwnershipProtocolError>;
    fn verify_binding(&self, root: &Path, task: &str) -> Result<(), OwnershipError>;
}

pub trait WorktreeOwnerProtocolInternal {
    fn matches_attempt(&self, task: &str, attempt: &str) -> bool;
}

impl WorktreeOwnerProtocolInternal for WorktreeOwnerProtocol {
    fn matches_attempt(&self, task: &str, attempt: &str) -> bool {
        self.version == 1 && self.task_id == task && self.attempt_id == attempt
    }
}

fn validate_launch_dispatch_binding(
    tx: &rusqlite::Transaction<'_>,
    launch_detail: &str,
    dispatch_detail: &str,
    task: &str,
    attempt: &str,
) -> Result<(), OwnershipProtocolError> {
    let launch: serde_json::Value =
        serde_json::from_str(launch_detail).map_err(|_| OwnershipProtocolError::Conflict)?;
    let launch_matches = launch.get("task_id").and_then(serde_json::Value::as_str) == Some(task)
        && launch.get("attempt_id").and_then(serde_json::Value::as_str) == Some(attempt);
    if !launch_matches
        || (dispatch_detail != launch_detail
            && !valid_amended_dispatch(tx, dispatch_detail, launch_detail, task, attempt))
    {
        return Err(OwnershipProtocolError::Conflict);
    }
    Ok(())
}

fn valid_amended_dispatch(
    tx: &rusqlite::Transaction<'_>,
    detail: &str,
    launch_detail: &str,
    task: &str,
    attempt: &str,
) -> bool {
    let Ok(dispatch) = serde_json::from_str::<serde_json::Value>(detail) else {
        return false;
    };
    let Some(sequence) = dispatch
        .get("amendment_sequence")
        .and_then(serde_json::Value::as_i64)
    else {
        return false;
    };
    let Some(plan) = dispatch.get("effective_plan") else {
        return false;
    };
    if sequence <= 0
        || !plan.is_object()
        || plan.get("task_id").and_then(serde_json::Value::as_str) != Some(task)
        || plan.get("attempt_id").and_then(serde_json::Value::as_str) != Some(attempt)
    {
        return false;
    }
    let Ok(rows) = tx
        .prepare("SELECT sequence,payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='initial_branch_removed'")
        .and_then(|mut statement| {
            statement
                .query_map(params![task, attempt], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
    else {
        return false;
    };
    let [(audit_sequence, payload)] = rows.as_slice() else {
        return false;
    };
    let (Ok(audit), Ok(original_plan)) = (
        serde_json::from_str::<serde_json::Value>(payload),
        serde_json::from_str::<serde_json::Value>(launch_detail),
    ) else {
        return false;
    };
    *audit_sequence == sequence
        && audit.get("task_id").and_then(serde_json::Value::as_str) == Some(task)
        && audit.get("attempt_id").and_then(serde_json::Value::as_str) == Some(attempt)
        && audit.get("effective_plan") == Some(plan)
        && audit
            .get("saved_launch_plan")
            .and_then(serde_json::Value::as_str)
            == Some(launch_detail)
        && audit.get("original_plan") == Some(&original_plan)
}

impl WorktreeOwnerInternal for WorktreeOwner {
    fn inherited(root: &Path, task: &str) -> Result<Self, OwnershipError> {
        let fd = std::env::var(FD_ENV)
            .map_err(|_| OwnershipError::Unavailable)?
            .parse::<i32>()
            .map_err(|_| OwnershipError::Unavailable)?;
        if fd < 3 || unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
            return Err(OwnershipError::Unavailable);
        }
        let owned_fd = unsafe { libc::dup(fd) };
        if owned_fd == -1 {
            return Err(OwnershipError::Unavailable);
        }
        let file = unsafe { File::from_raw_fd(owned_fd) };
        verify_file(root, task, &file)?;
        file.try_lock_exclusive().map_err(lock_error)?;
        verify_file(root, task, &file)?;
        Ok(Self { file })
    }

    fn verify_protocol(
        &self,
        connection: &Connection,
        root: &Path,
        task: &str,
        attempt: &str,
    ) -> Result<(), OwnershipProtocolError> {
        self.verify_binding(root, task)
            .map_err(|_| OwnershipProtocolError::Conflict)?;
        let current = self
            .protocol_evidence(root, task, attempt)
            .map_err(|_| OwnershipProtocolError::Conflict)?;
        let tx = connection.unchecked_transaction()?;
        let (evidence_count, dispatch_count, payload, launch_count, launch_detail, dispatch_detail): (i64, i64, Option<String>, i64, Option<String>, Option<String>) = tx.query_row(
            "SELECT
                (SELECT COUNT(*) FROM evidence WHERE kind='worktree_owner_protocol' AND task_id=?1 AND attempt_id=?2),
                (SELECT COUNT(*) FROM intents WHERE kind='supervisor_dispatch' AND task_id=?1 AND attempt_id=?2),
                (SELECT payload FROM evidence WHERE kind='worktree_owner_protocol' AND task_id=?1 AND attempt_id=?2),
                (SELECT COUNT(*) FROM intents WHERE kind='launch' AND task_id=?1 AND attempt_id=?2),
                (SELECT detail FROM intents WHERE kind='launch' AND task_id=?1 AND attempt_id=?2),
                (SELECT detail FROM intents WHERE kind='supervisor_dispatch' AND task_id=?1 AND attempt_id=?2)",
            params![task, attempt],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )?;
        if evidence_count != 1 || dispatch_count != 1 || launch_count != 1 {
            return Err(OwnershipProtocolError::Conflict);
        }
        let launch_detail = launch_detail.ok_or(OwnershipProtocolError::Conflict)?;
        let dispatch_detail = dispatch_detail.ok_or(OwnershipProtocolError::Conflict)?;
        validate_launch_dispatch_binding(&tx, &launch_detail, &dispatch_detail, task, attempt)?;
        let persisted: WorktreeOwnerProtocol =
            serde_json::from_str(payload.as_deref().ok_or(OwnershipProtocolError::Conflict)?)
                .map_err(|_| OwnershipProtocolError::Conflict)?;
        if persisted != current || !persisted.matches_attempt(task, attempt) {
            return Err(OwnershipProtocolError::Conflict);
        }
        tx.commit()?;
        Ok(())
    }

    fn verify_protocol_in_snapshot(
        &self,
        tx: &rusqlite::Transaction<'_>,
        root: &Path,
        task: &str,
        attempt: &str,
    ) -> Result<(), OwnershipProtocolError> {
        self.verify_binding(root, task)
            .map_err(|_| OwnershipProtocolError::Conflict)?;
        let current = self
            .protocol_evidence(root, task, attempt)
            .map_err(|_| OwnershipProtocolError::Conflict)?;
        let (count, dispatch, payload): (i64, i64, Option<String>) = tx.query_row(
            "SELECT (SELECT COUNT(*) FROM evidence WHERE kind='worktree_owner_protocol' AND task_id=?1 AND attempt_id=?2), (SELECT COUNT(*) FROM intents WHERE kind='supervisor_dispatch' AND task_id=?1 AND attempt_id=?2), (SELECT payload FROM evidence WHERE kind='worktree_owner_protocol' AND task_id=?1 AND attempt_id=?2)",
            params![task, attempt], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let persisted: WorktreeOwnerProtocol =
            serde_json::from_str(payload.as_deref().ok_or(OwnershipProtocolError::Conflict)?)
                .map_err(|_| OwnershipProtocolError::Conflict)?;
        if count != 1
            || dispatch != 1
            || persisted != current
            || !persisted.matches_attempt(task, attempt)
        {
            return Err(OwnershipProtocolError::Conflict);
        }
        Ok(())
    }

    fn verify_binding(&self, root: &Path, task: &str) -> Result<(), OwnershipError> {
        verify_file(root, task, &self.file)
    }
}

impl WorktreeOwner {
    pub fn acquire(root: &Path, task: &str) -> Result<Self, OwnershipError> {
        Self::open(root, task, true)
    }

    pub fn acquire_existing(root: &Path, task: &str) -> Result<Self, OwnershipError> {
        Self::open(root, task, false)
    }

    fn open(root: &Path, task: &str, create: bool) -> Result<Self, OwnershipError> {
        let path = lock_path(root, task)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| OwnershipError::Unavailable)?;
        verify_file(root, task, &file)?;
        file.try_lock_exclusive().map_err(lock_error)?;
        verify_file(root, task, &file)?;
        Ok(Self { file })
    }

    fn fd(&self) -> i32 {
        self.file.as_raw_fd()
    }

    pub fn inherit_into(&self, command: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;

        let fd = self.fd();
        command.env(FD_ENV, fd.to_string());
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    pub fn protocol_evidence(
        &self,
        root: &Path,
        task: &str,
        attempt: &str,
    ) -> Result<WorktreeOwnerProtocol, OwnershipError> {
        self.verify_binding(root, task)?;
        let metadata = self
            .file
            .metadata()
            .map_err(|_| OwnershipError::Unavailable)?;
        Ok(WorktreeOwnerProtocol {
            version: 1,
            task_id: task.to_owned(),
            attempt_id: attempt.to_owned(),
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }
}

fn lock_error(error: io::Error) -> OwnershipError {
    if error.kind() == io::ErrorKind::WouldBlock {
        OwnershipError::Busy
    } else {
        OwnershipError::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, sync::Mutex};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct EnvRestore(Option<std::ffi::OsString>);

    impl EnvRestore {
        fn new() -> Self {
            Self(std::env::var_os(FD_ENV))
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => unsafe { std::env::set_var(FD_ENV, value) },
                None => unsafe { std::env::remove_var(FD_ENV) },
            }
        }
    }

    #[test]
    fn lock_lifetime_is_bound_to_the_open_file_not_the_probe() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let first = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        assert!(matches!(
            WorktreeOwner::acquire(dir.path(), "task-a"),
            Err(OwnershipError::Busy)
        ));
        let other = WorktreeOwner::acquire(dir.path(), "task-b").unwrap();
        drop(first);
        WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        drop(other);
    }

    #[test]
    fn unsafe_ownership_paths_refuse_without_repair() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            WorktreeOwner::acquire(dir.path(), "../task"),
            Err(OwnershipError::Unavailable)
        ));
        let path = dir.path().join("worktree-task-a.lock");
        symlink("missing", &path).unwrap();
        assert!(matches!(
            WorktreeOwner::acquire(dir.path(), "task-a"),
            Err(OwnershipError::Unavailable)
        ));
        assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
        fs::remove_file(&path).unwrap();
        fs::write(&path, "").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            WorktreeOwner::acquire(dir.path(), "task-a"),
            Err(OwnershipError::Unavailable)
        ));
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o644);
    }

    fn root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn busy(root: &Path, task: &str) {
        assert!(matches!(
            WorktreeOwner::acquire(root, task),
            Err(OwnershipError::Busy)
        ));
    }

    #[test]
    fn binding_rejects_task_swap_hardlink_and_symlink() {
        let _guard = TEST_LOCK.lock().unwrap();
        let dir = root();
        let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        assert!(owner.verify_binding(dir.path(), "task-b").is_err());
        let path = dir.path().join("worktree-task-a.lock");
        let link = dir.path().join("hardlink");
        fs::hard_link(&path, &link).unwrap();
        assert!(owner.verify_binding(dir.path(), "task-a").is_err());
        fs::remove_file(link).unwrap();
        fs::remove_file(&path).unwrap();
        symlink("elsewhere", &path).unwrap();
        assert!(owner.verify_binding(dir.path(), "task-a").is_err());
    }

    #[test]
    fn binding_rejects_swapped_inode_and_bad_permissions() {
        let _guard = TEST_LOCK.lock().unwrap();
        let dir = root();
        let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        let path = dir.path().join("worktree-task-a.lock");
        fs::remove_file(&path).unwrap();
        fs::write(&path, "replacement").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(owner.verify_binding(dir.path(), "task-a").is_err());
        drop(owner);
        let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(owner.verify_binding(dir.path(), "task-a").is_err());
    }

    fn protocol_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE evidence(sequence INTEGER PRIMARY KEY, task_id TEXT NOT NULL, attempt_id TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL); CREATE TABLE intents(sequence INTEGER PRIMARY KEY, id TEXT, task_id TEXT NOT NULL, attempt_id TEXT, kind TEXT NOT NULL, detail TEXT NOT NULL);").unwrap();
        db
    }

    fn add_dispatch(db: &Connection) {
        let launch = serde_json::json!({
            "task_id": "task-a", "attempt_id": "attempt-a", "session_id": "persisted-session",
            "worktree": "/worktree",
            "expected_worktree": {"path": "/worktree", "device": 1, "inode": 1,
                "branch": "luthor/task-a", "base": "main", "head": "abc", "repository": "org/code",
                "git_directory": "/checkout/.git", "remote": "origin"},
            "executable": "/worker", "args": ["private-prompt"], "config_revision": "rev",
            "session_environment": {"home": "/", "xdg_config_home": null,
                "xdg_data_home": null, "xdg_state_home": null, "llxprt_config_home": null}
        });
        let detail = launch.to_string();
        db.execute("INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('l','task-a','attempt-a','launch',?1)", [&detail]).unwrap();
        db.execute("INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('i','task-a','attempt-a','supervisor_dispatch',?1)", [&detail]).unwrap();
    }

    fn add_protocol(db: &Connection, proof: &WorktreeOwnerProtocol) {
        db.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-a','attempt-a','worktree_owner_protocol',?1)", [serde_json::to_string(proof).unwrap()]).unwrap();
    }

    #[test]
    fn amended_dispatch_matches_persisted_audit_sequence_and_original_launch() {
        let db = protocol_db();
        let launch =
            serde_json::json!({"task_id":"task-a","attempt_id":"attempt-a","args":["original"]});
        let launch_detail = launch.to_string();
        let plan =
            serde_json::json!({"task_id":"task-a","attempt_id":"attempt-a","args":["amended"]});
        let dispatch =
            serde_json::json!({"amendment_sequence":7,"effective_plan":plan.clone()}).to_string();
        let audit =
            serde_json::json!({"task_id":"task-a","attempt_id":"attempt-a","effective_plan":plan,
            "saved_launch_plan":launch_detail.clone(),"original_plan":launch.clone()})
            .to_string();
        db.execute("INSERT INTO evidence(sequence,task_id,attempt_id,kind,payload) VALUES(7,'task-a','attempt-a','initial_branch_removed',?1)", [&audit]).unwrap();
        let tx = db.unchecked_transaction().unwrap();
        assert!(valid_amended_dispatch(
            &tx,
            &dispatch,
            &launch_detail,
            "task-a",
            "attempt-a"
        ));
        tx.rollback().unwrap();

        let wrong_original = audit.replace("original", "changed");
        db.execute("UPDATE evidence SET payload=?1", [&wrong_original])
            .unwrap();
        let tx = db.unchecked_transaction().unwrap();
        assert!(!valid_amended_dispatch(
            &tx,
            &dispatch,
            &launch_detail,
            "task-a",
            "attempt-a"
        ));
        tx.rollback().unwrap();
        db.execute("INSERT INTO evidence(sequence,task_id,attempt_id,kind,payload) SELECT 8,task_id,attempt_id,kind,payload FROM evidence WHERE sequence=7", []).unwrap();
        let tx = db.unchecked_transaction().unwrap();
        assert!(!valid_amended_dispatch(
            &tx,
            &dispatch,
            &launch_detail,
            "task-a",
            "attempt-a"
        ));
    }

    #[test]
    fn protocol_evidence_requires_real_matching_owner() {
        let _guard = TEST_LOCK.lock().unwrap();
        let dir = root();
        let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        let proof = owner
            .protocol_evidence(dir.path(), "task-a", "attempt-a")
            .unwrap();
        assert_eq!(proof.task_id, "task-a");
        assert!(
            owner
                .protocol_evidence(dir.path(), "task-b", "attempt-a")
                .is_err()
        );
        let db = protocol_db();
        add_protocol(&db, &proof);
        add_dispatch(&db);
        assert!(
            owner
                .verify_protocol(&db, dir.path(), "task-a", "attempt-a")
                .is_ok()
        );
    }

    #[test]
    fn verify_protocol_rejects_invalid_proof_and_dispatch() {
        let _guard = TEST_LOCK.lock().unwrap();
        let dir = root();
        let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        let proof = owner
            .protocol_evidence(dir.path(), "task-a", "attempt-a")
            .unwrap();
        for case in 0..8 {
            let db = protocol_db();
            if case >= 6 {
                add_protocol(&db, &proof);
                add_dispatch(&db);
            }
            match case {
                0 => {}
                1 => {
                    add_protocol(&db, &proof);
                    add_protocol(&db, &proof);
                    add_dispatch(&db);
                }
                2 => {
                    db.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-a','attempt-a','worktree_owner_protocol','{')", []).unwrap();
                    add_dispatch(&db);
                }
                3 => {
                    let mut p = proof.clone();
                    p.version = 2;
                    add_protocol(&db, &p);
                    add_dispatch(&db);
                }
                4 => {
                    let mut p = proof.clone();
                    p.ino += 1;
                    add_protocol(&db, &p);
                    add_dispatch(&db);
                }
                5 => {
                    add_protocol(&db, &proof);
                }
                6 => {
                    db.execute(
                        "UPDATE intents SET detail='{' WHERE kind='supervisor_dispatch'",
                        [],
                    )
                    .unwrap();
                }
                7 => {
                    let mismatched =
                        serde_json::json!({"task_id": "task-a", "attempt_id": "attempt-other"});
                    db.execute(
                        "UPDATE intents SET detail=?1 WHERE kind='supervisor_dispatch'",
                        [mismatched.to_string()],
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                owner
                    .verify_protocol(&db, dir.path(), "task-a", "attempt-a")
                    .is_err(),
                "case {case}"
            );
        }
    }

    #[test]
    fn inherited_duplicate_keeps_lock_after_owner_descriptor_closes() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let dir = root();
        let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        let fd = unsafe { libc::dup(owner.fd()) };
        assert!(fd >= 0);
        let inherited = unsafe { File::from_raw_fd(fd) };
        drop(owner);
        busy(dir.path(), "task-a");
        drop(inherited);
        WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
    }

    #[test]
    fn inherited_lock_does_not_block_another_worktree() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let dir = root();
        let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        let fd = unsafe { libc::dup(owner.fd()) };
        assert!(fd >= 0);
        let inherited = unsafe { File::from_raw_fd(fd) };
        drop(owner);
        busy(dir.path(), "task-a");
        drop(WorktreeOwner::acquire(dir.path(), "task-b").unwrap());
        drop(inherited);
    }

    #[test]
    fn inherited_refuses_independently_opened_busy_descriptor() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let _restore = EnvRestore::new();
        let dir = root();
        let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        let independent = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join("worktree-task-a.lock"))
            .unwrap();
        unsafe { std::env::set_var(FD_ENV, independent.as_raw_fd().to_string()) };
        assert!(matches!(
            WorktreeOwner::inherited(dir.path(), "task-a"),
            Err(OwnershipError::Busy)
        ));
        drop(owner);
        drop(independent);
    }

    #[test]
    fn inherited_refuses_closed_and_wrong_descriptors() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let _restore = EnvRestore::new();
        let dir = root();
        let _owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        let closed_fd = i32::MAX;
        unsafe { std::env::set_var(FD_ENV, closed_fd.to_string()) };
        assert!(matches!(
            WorktreeOwner::inherited(dir.path(), "task-a"),
            Err(OwnershipError::Unavailable)
        ));
        let unrelated = tempfile::tempfile().unwrap();
        let unrelated_fd = unsafe { libc::dup(unrelated.as_raw_fd()) };
        assert!(unrelated_fd >= 0);
        unsafe { std::env::set_var(FD_ENV, unrelated_fd.to_string()) };
        assert!(matches!(
            WorktreeOwner::inherited(dir.path(), "task-a"),
            Err(OwnershipError::Unavailable)
        ));
        drop(unrelated);
    }

    #[test]
    fn inherited_refuses_replaced_lock_file_without_repairing_it() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let _restore = EnvRestore::new();
        let dir = root();
        let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        let fd = unsafe { libc::dup(owner.fd()) };
        assert!(fd >= 0);
        let path = dir.path().join("worktree-task-a.lock");
        fs::remove_file(&path).unwrap();
        fs::write(&path, "replacement").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        unsafe { std::env::set_var(FD_ENV, fd.to_string()) };
        assert!(matches!(
            WorktreeOwner::inherited(dir.path(), "task-a"),
            Err(OwnershipError::Unavailable)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        drop(owner);
    }

    #[test]
    fn command_inherits_owner_fd_without_changing_parent_flags() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let dir = root();
        let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        let fd = owner.fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_ne!(flags, -1);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);

        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 0.2"]);
        owner.inherit_into(&mut command);
        let mut child = command.spawn().unwrap();
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, flags);
        drop(owner);

        busy(dir.path(), "task-a");
        drop(WorktreeOwner::acquire(dir.path(), "task-b").unwrap());
        assert!(child.wait().unwrap().success());
        WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
    }

    #[test]
    fn surviving_descendant_keeps_lock_after_owner_parent_drops_it() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let dir = root();
        let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            unsafe {
                libc::close(pipe[1]);
                let mut byte = 0u8;
                let result = libc::read(pipe[0], &mut byte as *mut u8 as *mut _, 1);
                libc::_exit(if result == 1 { 0 } else { 1 });
            }
        }
        unsafe { libc::close(pipe[0]) };
        drop(owner);
        busy(dir.path(), "task-a");
        assert_eq!(
            unsafe { libc::write(pipe[1], b"x".as_ptr() as *const _, 1) },
            1
        );
        unsafe { libc::close(pipe[1]) };
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
        WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
    }
}
