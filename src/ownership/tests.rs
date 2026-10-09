use super::*;
use std::{io::BufRead, os::unix::fs::symlink, sync::Mutex};

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
fn acquire_existing_refuses_trailing_lock_bytes_without_repairing_file() {
    let _guard = TEST_LOCK.lock().unwrap();
    let dir = root();
    let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
    let path = dir.path().join("worktree-task-a.lock");
    drop(owner);

    let mut original = fs::read(&path).unwrap();
    assert_eq!(original.len(), NONCE_PREFIX.len() + NONCE_LEN * 2 + 1);
    original.push(b'x');
    fs::write(&path, &original).unwrap();

    assert!(matches!(
        WorktreeOwner::acquire_existing(dir.path(), "task-a"),
        Err(OwnershipError::Unavailable)
    ));
    assert_eq!(fs::read(&path).unwrap(), original);
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
    let result = WorktreeOwner::acquire(root, task);
    assert!(
        matches!(result, Err(OwnershipError::Busy)),
        "got {}",
        match result {
            Ok(_) => "Ok",
            Err(OwnershipError::Busy) => "Busy",
            Err(OwnershipError::Unavailable) => "Unavailable",
        }
    );
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
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(owner.verify_binding(dir.path(), "task-a").is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(&path, "replacement").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
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
    let plan = serde_json::json!({"task_id":"task-a","attempt_id":"attempt-a","args":["amended"]});
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
                p.version = 1;
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
fn verify_protocol_rejects_persisted_proof_with_wrong_nonce_on_same_inode() {
    let _guard = TEST_LOCK.lock().unwrap();
    let dir = root();
    let owner = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
    let proof = owner
        .protocol_evidence(dir.path(), "task-a", "attempt-a")
        .unwrap();
    let mut stale = proof.clone();
    stale
        .nonce
        .replace_range(..1, if &stale.nonce[..1] == "0" { "1" } else { "0" });
    let db = protocol_db();
    add_protocol(&db, &stale);
    add_dispatch(&db);
    assert!(
        owner
            .verify_protocol(&db, dir.path(), "task-a", "attempt-a")
            .is_err()
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM evidence", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn replacement_lock_nonce_rejects_previously_persisted_proof_without_mutation() {
    let _guard = TEST_LOCK.lock().unwrap();
    let dir = root();
    let original = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
    let persisted = original
        .protocol_evidence(dir.path(), "task-a", "attempt-a")
        .unwrap();
    let db = protocol_db();
    add_protocol(&db, &persisted);
    add_dispatch(&db);
    drop(original);
    let path = dir.path().join("worktree-task-a.lock");
    fs::remove_file(&path).unwrap();
    let replacement = WorktreeOwner::acquire(dir.path(), "task-a").unwrap();
    let current = replacement
        .protocol_evidence(dir.path(), "task-a", "attempt-a")
        .unwrap();
    assert_ne!(current.nonce, persisted.nonce);
    assert!(
        replacement
            .verify_protocol(&db, dir.path(), "task-a", "attempt-a")
            .is_err()
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM evidence", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM intents", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
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
    command.args(["-c", "printf 'ready\\n'; exec sleep 30"]);
    owner.inherit_into(&mut command);
    command.stdout(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut ready = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, flags);
    drop(owner);

    busy(dir.path(), "task-a");
    drop(WorktreeOwner::acquire(dir.path(), "task-b").unwrap());
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
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
