//! A cooperative, process-lifetime lock for a task's worktree.
//! The state-root coordinator lock serializes decisions; this lock survives it
//! when an already-dispatched worker continues after the coordinator exits.
use fs2::FileExt;
use rusqlite::{Connection, params};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::FileExt as StdFileExt,
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
    nonce: String,
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

const NONCE_PREFIX: &[u8] = b"LUTHOR-OWNER-2:";
const NONCE_LEN: usize = 32;

fn nonce_bytes(file: &File) -> Result<[u8; NONCE_LEN], OwnershipError> {
    let mut contents = Vec::new();
    let mut offset = 0;
    while offset < 80 {
        let mut chunk = [0; 80];
        let count = file
            .read_at(&mut chunk, offset as u64)
            .map_err(|_| OwnershipError::Unavailable)?;
        if count == 0 {
            break;
        }
        contents.extend_from_slice(&chunk[..count]);
        offset += count;
    }
    if contents.len() != NONCE_PREFIX.len() + NONCE_LEN * 2 + 1
        || !contents.starts_with(NONCE_PREFIX)
        || contents.last() != Some(&b'\n')
    {
        return Err(OwnershipError::Unavailable);
    }
    let mut nonce = [0; NONCE_LEN];
    for (index, pair) in contents[NONCE_PREFIX.len()..contents.len() - 1]
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
    {
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        nonce[index] = (digit(pair[0]).ok_or(OwnershipError::Unavailable)? << 4)
            | digit(pair[1]).ok_or(OwnershipError::Unavailable)?;
    }
    Ok(nonce)
}

fn verify_metadata(root: &Path, task: &str, file: &File) -> Result<(), OwnershipError> {
    let path = lock_path(root, task)?;
    let opened = file.metadata().map_err(|_| OwnershipError::Unavailable)?;
    let named = fs::symlink_metadata(&path).map_err(|_| OwnershipError::Unavailable)?;
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

fn verify_file(root: &Path, task: &str, file: &File) -> Result<[u8; NONCE_LEN], OwnershipError> {
    verify_metadata(root, task, file)?;
    let path = lock_path(root, task)?;
    let opened = file.metadata().map_err(|_| OwnershipError::Unavailable)?;
    if opened.len() != (NONCE_PREFIX.len() + NONCE_LEN * 2 + 1) as u64 {
        return Err(OwnershipError::Unavailable);
    }
    let nonce = nonce_bytes(file)?;
    let after = fs::symlink_metadata(path).map_err(|_| OwnershipError::Unavailable)?;
    if (opened.dev(), opened.ino()) != (after.dev(), after.ino()) || nonce_bytes(file)? != nonce {
        return Err(OwnershipError::Unavailable);
    }
    Ok(nonce)
}

fn initialize_nonce(file: &mut File) -> Result<(), OwnershipError> {
    let mut nonce = [0; NONCE_LEN];
    File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut nonce))
        .map_err(|_| OwnershipError::Unavailable)?;
    let mut contents = Vec::with_capacity(NONCE_PREFIX.len() + NONCE_LEN * 2 + 1);
    contents.extend_from_slice(NONCE_PREFIX);
    for byte in nonce {
        contents.extend_from_slice(format!("{byte:02x}").as_bytes());
    }
    contents.push(b'\n');
    file.write_all(&contents)
        .map_err(|_| OwnershipError::Unavailable)?;
    file.sync_all().map_err(|_| OwnershipError::Unavailable)
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
        self.version == 2 && self.task_id == task && self.attempt_id == attempt
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
        verify_metadata(root, task, &file)?;
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
        verify_file(root, task, &self.file).map(|_| ())
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
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
        let (mut file, newly_created) = if create {
            match options.create_new(true).open(&path) {
                Ok(file) => (file, true),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (
                    options
                        .create_new(false)
                        .open(&path)
                        .map_err(|_| OwnershipError::Unavailable)?,
                    false,
                ),
                Err(_) => return Err(OwnershipError::Unavailable),
            }
        } else {
            (
                options
                    .open(&path)
                    .map_err(|_| OwnershipError::Unavailable)?,
                false,
            )
        };
        verify_metadata(root, task, &file)?;
        file.try_lock_exclusive().map_err(lock_error)?;
        if newly_created {
            initialize_nonce(&mut file)?;
        }
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
            version: 2,
            task_id: task.to_owned(),
            attempt_id: attempt.to_owned(),
            dev: metadata.dev(),
            ino: metadata.ino(),
            nonce: nonce_bytes(&self.file)?
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
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
mod tests;
