//! Synthetic predecessors only: never inspect or exercise either live attempt.
#![cfg(any(target_os = "macos", target_os = "linux"))]
mod supervisor_fixture;
use supervisor_fixture as configuration;
mod git_fixture;
mod historical_fixture;
use configuration::{claimed, configured};
use historical_fixture::{
    NoProjects, NoPrs, test_process_identity, wait_for_process_and_group_absence,
};
use luthor::supervisor::LaunchPlan;
use luthor::{
    WorktreeOwner,
    coordinator::{RecoveryResult, abandon_task, operator_recover_missing_receipt},
    state::{
        journal, launches,
        retirement::{self, RetiredPhase, Retirement},
        scheduling, task_records,
    },
};
use luthor::{
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    eligibility::Candidate,
    state::{StateError, StateStore},
    supervisor::{Reconciliation, prepare_initial, prepare_resume, reconcile_attempt},
    worktree::ensure_worktree,
};
use rusqlite::Connection;
use serde_json::json;
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, MutexGuard},
};

// All ownership/fork fixtures in this integration binary share this guard.
static FIXTURES: Mutex<()> = Mutex::new(());

struct Historical {
    _dir: tempfile::TempDir,
    config: Config,
    store: StateStore,
    plan: LaunchPlan,
    // Release only after state and fixture files are dropped.
    _isolation: MutexGuard<'static, ()>,
}

impl Historical {
    fn new() -> Self {
        let isolation = FIXTURES.lock().unwrap();
        let (dir, config, mut store) = claimed_store();
        let plan = prepare_initial(&mut store, "task", "attempt-historical").unwrap();
        Self {
            _dir: dir,
            config,
            store,
            plan,
            _isolation: isolation,
        }
    }

    fn db(&self) -> Connection {
        Connection::open(self.store.root().join("state.sqlite3")).unwrap()
    }

    fn predates_preparation(&self) {
        self.db()
            .execute("DELETE FROM launch_preparations", [])
            .unwrap();
    }

    fn attempts(&self) -> PathBuf {
        self.config.state_root.join("attempts")
    }

    fn managed_dispatch(&mut self) {
        luthor::supervisor::preflight_attempt_storage(self.store.root()).unwrap();
        let path = self.attempts().join("attempt-historical.plan.json");
        fs::write(&path, serde_json::to_vec(&self.plan).unwrap()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        let owner = WorktreeOwner::acquire_existing(self.store.root(), "task").unwrap();
        let proof = owner
            .protocol_evidence(self.store.root(), "task", &self.plan.attempt_id)
            .unwrap();
        launches::begin_supervision(
            &mut self.store,
            "task",
            &self.plan.attempt_id,
            &serde_json::to_string(&self.plan).unwrap(),
            &proof,
        )
        .unwrap();
    }

    fn assert_retirement_refused(&mut self) {
        for phase in RetiredPhase::ALL {
            assert!(
                retirement::retire(
                    &mut self.store,
                    "task",
                    &Retirement {
                        phase,
                        actor: Some("operator".into()),
                        reason: "external closure is not quiescence".into(),
                        observed_at_unix_secs: 1,
                        evidence: json!({"issue": "closed", "pr": "merged"}),
                    }
                )
                .is_err()
            );
        }
    }

    fn assert_hold(&mut self) {
        let before = rows(&self.db());
        let mut projects = NoProjects;
        let mut prs = NoPrs;
        assert!(matches!(
            operator_recover_missing_receipt(
                &mut self.store,
                "task",
                &self.plan.attempt_id,
                "operator",
                "audited absence is not proof",
                &mut projects,
                &mut prs
            )
            .unwrap(),
            RecoveryResult::Held(_)
        ));
        assert!(matches!(
            reconcile_attempt(&mut self.store, "task", &self.plan.attempt_id).unwrap(),
            Reconciliation::Held { .. }
        ));
        assert!(
            abandon_task(
                &mut self.store,
                "task",
                "operator",
                "retire queue, not slot"
            )
            .is_err()
        );
        self.assert_retirement_refused();
        assert!(prepare_resume(&mut self.store, "task", "attempt-resume").is_err());
        assert!(
            luthor::state::retry_plan_for_task(&self.store, "task", &self.plan.attempt_id).is_err()
        );
        assert!(
            self.store
                .never_dispatched_context("task", &self.plan.attempt_id)
                .is_err()
        );
        assert!(scheduling::release_reservation(&mut self.store, &self.plan.attempt_id).is_err());
        assert_eq!(scheduling::reservation_count(&self.store).unwrap(), 1);
        assert_eq!(
            task_records::task_phase(&self.store, "task")
                .unwrap()
                .as_deref(),
            Some("held")
        );
        // Arithmetic leaves one slot, but does not authorize dispatch or clear the pending attempt.
        assert!(scheduling::ensure_dispatch_capacity(&self.store).is_ok());
        assert_eq!(
            scheduling::pending_attempts(&self.store).unwrap(),
            vec![("task".into(), self.plan.attempt_id.clone())]
        );
        assert_eq!(
            rows(&self.db()),
            before,
            "no fabricated history or reservation transition"
        );
    }
}

fn claimed_store() -> (tempfile::TempDir, Config, StateStore) {
    let dir = tempfile::tempdir().unwrap();
    let (mut config, candidate) = configured(dir.path());
    config.capacity = 2;
    let mut store = StateStore::open(&config.state_root, 2).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    (dir, config, store)
}

fn rows(db: &Connection) -> Vec<Vec<String>> {
    [
        "SELECT json_array(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision,created_at) FROM tasks ORDER BY rowid",
        "SELECT json_array(id,task_id,lifecycle,outcome,created_at) FROM attempts ORDER BY rowid",
        "SELECT json_array(attempt_id,task_id,status,created_at) FROM reservations ORDER BY rowid",
        "SELECT json_array(sequence,id,task_id,attempt_id,kind,detail,created_at) FROM intents ORDER BY sequence",
        "SELECT json_array(sequence,task_id,attempt_id,kind,payload,created_at) FROM evidence ORDER BY sequence",
        "SELECT json_array(attempt_id,task_id,launch_sequence,owner_payload) FROM launch_preparations ORDER BY rowid",
        "SELECT json_array(name,seq) FROM sqlite_sequence ORDER BY name",
    ].iter().map(|query| db.prepare(query).unwrap().query_map([], |row| row.get(0))
        .unwrap().collect::<Result<Vec<_>, _>>().unwrap()).collect()
}

#[test]
fn historical_launch_without_owner_stays_held_after_storage_repair_and_fresh_lock() {
    let mut f = Historical::new();
    f.predates_preparation();
    fs::remove_file(f.store.root().join("worktree-task.lock")).unwrap();
    f.plan.worker_build = None;
    f.plan
        .args
        .extend(["--branch".into(), "luthor/task".into()]);
    let original = serde_json::to_string_pretty(&f.plan).unwrap();
    f.db()
        .execute(
            "UPDATE intents SET detail=?1 WHERE kind='launch'",
            [&original],
        )
        .unwrap();
    fs::create_dir(f.attempts()).unwrap();
    fs::set_permissions(f.attempts(), fs::Permissions::from_mode(0o755)).unwrap();
    f.assert_hold();
    assert!(luthor::supervisor::preflight_attempt_storage(f.store.root()).is_err());
    fs::set_permissions(f.attempts(), fs::Permissions::from_mode(0o700)).unwrap();
    luthor::supervisor::preflight_attempt_storage(f.store.root()).unwrap();
    drop(WorktreeOwner::acquire(f.store.root(), "task").unwrap());
    for _ in 0..3 {
        f.assert_hold();
    }
    assert_eq!(fs::read_dir(f.attempts()).unwrap().count(), 0);
    assert_eq!(
        launches::launch_intent(&f.store, &f.plan.attempt_id)
            .unwrap()
            .unwrap(),
        original
    );
}

#[test]
fn historical_broken_pipe_with_owner_and_absent_child_is_not_a_dispatch_abort() {
    let mut f = Historical::new();
    f.managed_dispatch();
    f.predates_preparation();
    let mut child = Command::new("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let (boot, start) = test_process_identity(child.id());
    let identity = json!({"pid": child.id(), "group_id": child.id(), "boot_identity": boot, "start_identity": start});
    let path = f.attempts().join("attempt-historical.child.json");
    fs::write(&path, identity.to_string()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    journal::record_evidence(
        &mut f.store,
        "task",
        Some(&f.plan.attempt_id),
        "child_registered",
        &identity.to_string(),
    )
    .unwrap();
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    wait_for_process_and_group_absence(child.id() as i32);
    for suffix in ["stdout.log", "stderr.log"] {
        fs::write(
            f.attempts().join(format!("attempt-historical.{suffix}")),
            "",
        )
        .unwrap();
    }
    let socket = f.attempts().join("attempt-historical.stop.sock");
    drop(std::os::unix::net::UnixListener::bind(socket).unwrap());
    fs::write(
        f.attempts().join("attempt-historical.supervisor.log"),
        "panicked: failed printing to stdout: Broken pipe",
    )
    .unwrap();
    let before = fs::read(path).unwrap();
    for _ in 0..3 {
        f.assert_hold();
    }
    assert_eq!(
        fs::read(f.attempts().join("attempt-historical.child.json")).unwrap(),
        before
    );
    assert!(
        !f.attempts()
            .join("attempt-historical.dispatch-aborted.json")
            .exists()
    );
}

#[test]
fn unregistered_live_process_and_free_new_lock_never_authorize_historical_abandonment() {
    let mut f = Historical::new();
    f.predates_preparation();
    let mut process = Command::new("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let identity = test_process_identity(process.id());
    f.assert_hold();
    assert_eq!(
        test_process_identity(process.id()),
        identity,
        "unaccounted process was not signalled"
    );
    drop(process.stdin.take());
    assert!(process.wait().unwrap().success());
    wait_for_process_and_group_absence(process.id() as i32);
    f.assert_hold();
}

#[test]
fn historical_storage_faults_and_repairs_never_create_preparation_evidence() {
    let mut f = Historical::new();
    f.predates_preparation();
    let attempts = f.attempts();
    for storage in ["file", "symlink", "public", "private"] {
        match storage {
            "file" => fs::write(&attempts, "unsafe storage").unwrap(),
            "symlink" => {
                std::os::unix::fs::symlink(f.config.worktree_root.clone(), &attempts).unwrap()
            }
            _ => {
                fs::create_dir(&attempts).unwrap();
                fs::set_permissions(
                    &attempts,
                    fs::Permissions::from_mode(if storage == "public" { 0o755 } else { 0o700 }),
                )
                .unwrap();
            }
        }
        let mode = fs::symlink_metadata(&attempts)
            .unwrap()
            .permissions()
            .mode();
        f.assert_hold();
        assert_eq!(
            fs::symlink_metadata(&attempts)
                .unwrap()
                .permissions()
                .mode(),
            mode
        );
        if storage == "file" || storage == "symlink" {
            fs::remove_file(&attempts).unwrap();
        } else {
            fs::remove_dir(&attempts).unwrap();
        }
    }
    f.assert_hold();
}

#[test]
fn historical_proof_database_errors_are_errors_not_successful_disposition() {
    let mut f = Historical::new();
    f.predates_preparation();
    let before = rows(&f.db());
    f.db()
        .execute_batch("ALTER TABLE launch_preparations RENAME TO unavailable_preparations")
        .unwrap();
    assert!(matches!(
        f.store.never_dispatched_context("task", &f.plan.attempt_id),
        Err(StateError::Sql(_))
    ));
    assert!(matches!(
        abandon_task(&mut f.store, "task", "operator", "unreadable evidence"),
        Err(StateError::Sql(_))
    ));
    f.db()
        .execute_batch("ALTER TABLE unavailable_preparations RENAME TO launch_preparations")
        .unwrap();
    assert_eq!(rows(&f.db()), before);
    f.assert_hold();
}

#[test]
fn contemporary_preparation_can_abandon_only_the_exact_unreleased_attempt() {
    let mut f = Historical::new();
    let before = rows(&f.db());
    let saved: String = f
        .db()
        .query_row("SELECT owner_payload FROM launch_preparations", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut proof: serde_json::Value = serde_json::from_str(&saved).unwrap();
    proof["attempt_id"] = "other-attempt".into();
    f.db()
        .execute(
            "UPDATE launch_preparations SET owner_payload=?1",
            [proof.to_string()],
        )
        .unwrap();
    assert!(abandon_task(&mut f.store, "task", "operator", "mismatched proof").is_err());
    f.db()
        .execute("UPDATE launch_preparations SET owner_payload=?1", [&saved])
        .unwrap();
    assert_eq!(rows(&f.db()), before);
    abandon_task(
        &mut f.store,
        "task",
        "operator",
        "prepared but never dispatched",
    )
    .unwrap();
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    let after = rows(&f.db());
    assert_eq!(after[1], before[1]);
    assert_eq!(after[3], before[3]);
    assert_eq!(after[5], before[5]);
    assert_eq!(after[4].len(), before[4].len() + 1);
    assert!(abandon_task(&mut f.store, "task", "operator", "replay").is_err());
    assert_eq!(rows(&f.db()), after);
}

#[test]
fn preparation_write_failure_rolls_back_launch_reservation_and_history() {
    let _isolation = FIXTURES.lock().unwrap();
    let (_dir, config, mut store) = claimed_store();
    let db = Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let before = rows(&db);
    db.execute_batch(
        "CREATE TRIGGER reject_preparation BEFORE INSERT ON launch_preparations
        BEGIN SELECT RAISE(ABORT,'injected preparation failure'); END;",
    )
    .unwrap();
    assert!(prepare_initial(&mut store, "task", "attempt-historical").is_err());
    assert_eq!(rows(&db), before);
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    db.execute_batch("DROP TRIGGER reject_preparation").unwrap();
    prepare_initial(&mut store, "task", "attempt-historical").unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(rows(&db)[5].len(), 1);
}

#[test]
fn replaced_owner_lock_cannot_adopt_a_contemporary_preparation() {
    let mut f = Historical::new();
    let before = rows(&f.db());
    fs::remove_file(f.store.root().join("worktree-task.lock")).unwrap();
    drop(WorktreeOwner::acquire(f.store.root(), "task").unwrap());
    assert!(
        abandon_task(
            &mut f.store,
            "task",
            "operator",
            "new lock is not old ownership"
        )
        .is_err()
    );
    let owner = WorktreeOwner::acquire_existing(f.store.root(), "task").unwrap();
    let proof = owner
        .protocol_evidence(f.store.root(), "task", &f.plan.attempt_id)
        .unwrap();
    assert!(
        launches::begin_supervision(
            &mut f.store,
            "task",
            &f.plan.attempt_id,
            &serde_json::to_string(&f.plan).unwrap(),
            &proof
        )
        .is_err()
    );
    assert_eq!(rows(&f.db()), before);
}

#[test]
fn historical_crash_child() {
    let Ok(path) = std::env::var("ISSUE27_CRASH_CONFIG") else {
        return;
    };
    let config: Config = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let mut store = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert!(
        abandon_task(
            &mut store,
            "task",
            "operator",
            "cannot infer never-dispatched"
        )
        .is_err()
    );
    assert!(matches!(
        luthor::supervisor::inspect_recovery_quiescence(&store, "task", "attempt-historical")
            .unwrap(),
        luthor::supervisor::RecoveryInspection::Held(_)
    ));
    assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGKILL) }, 0);
    unreachable!("SIGKILL returned");
}

#[test]
fn historical_hold_and_exact_history_survive_repeated_sigkill_and_reopen() {
    use std::os::unix::process::ExitStatusExt;
    for dispatched in [false, true] {
        let mut f = Historical::new();
        if dispatched {
            f.managed_dispatch();
        }
        f.predates_preparation();
        let original = launches::launch_intent(&f.store, &f.plan.attempt_id).unwrap();
        let before = rows(&f.db());
        let config_path = f._dir.path().join("crash-config.json");
        fs::write(&config_path, serde_json::to_vec(&f.config).unwrap()).unwrap();
        for _ in 0..3 {
            let unused = StateStore::open(f._dir.path().join("unused"), 2).unwrap();
            drop(std::mem::replace(&mut f.store, unused));
            let out = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "historical_crash_child", "--nocapture"])
                .env("ISSUE27_CRASH_CONFIG", &config_path)
                .output()
                .unwrap();
            assert_eq!(out.status.signal(), Some(libc::SIGKILL), "{out:?}");
            f.store = StateStore::open(&f.config.state_root, 2).unwrap();
            assert_eq!(rows(&f.db()), before);
            assert_eq!(
                launches::launch_intent(&f.store, &f.plan.attempt_id).unwrap(),
                original
            );
            f.assert_hold();
        }
    }
}

#[test]
fn historical_intent_cannot_gain_dispatch_proof_from_a_new_owner() {
    let mut f = Historical::new();
    f.predates_preparation();
    let before = rows(&f.db());
    let owner = WorktreeOwner::acquire_existing(f.store.root(), "task").unwrap();
    let proof = owner
        .protocol_evidence(f.store.root(), "task", &f.plan.attempt_id)
        .unwrap();
    assert!(
        launches::begin_supervision(
            &mut f.store,
            "task",
            &f.plan.attempt_id,
            &serde_json::to_string(&f.plan).unwrap(),
            &proof
        )
        .is_err()
    );
    assert_eq!(rows(&f.db()), before);
}

#[test]
fn recovery_ownership_database_failure_is_an_error_not_an_absence_based_hold() {
    let mut f = Historical::new();
    f.managed_dispatch();
    f.predates_preparation();
    let before = rows(&f.db());
    f.db()
        .execute_batch("ALTER TABLE intents RENAME TO unavailable_intents")
        .unwrap();
    assert!(matches!(
        luthor::supervisor::inspect_recovery_quiescence(&f.store, "task", &f.plan.attempt_id),
        Err(luthor::supervisor::SupervisorError::Sql(_))
    ));
    f.db()
        .execute_batch("ALTER TABLE unavailable_intents RENAME TO intents")
        .unwrap();
    assert_eq!(rows(&f.db()), before);
    f.assert_hold();
}

#[test]
fn historical_continuation_and_amendment_refuse_before_remote_reads_or_launch() {
    use luthor::coordinator::{
        ContinuationDependencies, ContinuationRefusal, ContinuationResult,
        OsContinuationLocalInspector, OsContinuationProcessInspector,
        amend_never_dispatched_initial_branch, continue_never_dispatched,
    };
    for dispatched in [false, true] {
        let mut f = Historical::new();
        if dispatched {
            f.managed_dispatch();
        }
        f.predates_preparation();
        let before = rows(&f.db());
        for _ in 0..3 {
            let result = continue_never_dispatched(
                &mut f.store,
                ContinuationDependencies {
                    task_id: "task",
                    attempt_id: &f.plan.attempt_id,
                    config: &f.config,
                    config_revision: "rev",
                    actor: "operator",
                    projects: &mut NoProjects,
                    prs: &mut NoPrs,
                    launcher: &mut historical_fixture::NoLaunch,
                    local: &mut OsContinuationLocalInspector,
                    processes: &mut OsContinuationProcessInspector,
                },
            )
            .unwrap();
            assert_eq!(
                result,
                ContinuationResult::Held(ContinuationRefusal::Ineligible)
            );
            let amended = amend_never_dispatched_initial_branch(
                &mut f.store,
                ContinuationDependencies {
                    task_id: "task",
                    attempt_id: &f.plan.attempt_id,
                    config: &f.config,
                    config_revision: "rev",
                    actor: "operator",
                    projects: &mut NoProjects,
                    prs: &mut NoPrs,
                    launcher: &mut historical_fixture::NoLaunch,
                    local: &mut OsContinuationLocalInspector,
                    processes: &mut OsContinuationProcessInspector,
                },
            )
            .unwrap();
            assert_eq!(
                amended,
                ContinuationResult::Held(ContinuationRefusal::Ineligible)
            );
            assert_eq!(rows(&f.db()), before);
            assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
        }
    }
}

#[test]
fn historical_process_fixture_distinguishes_live_identity_from_reaped_child() {
    let _isolation = FIXTURES.lock().unwrap();
    let mut child = Command::new("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let identity = test_process_identity(child.id());
    assert!(!identity.0.is_empty());
    assert!(!identity.1.is_empty());
    assert_eq!(
        historical_fixture::observed_process_identity(child.id()),
        Some(identity)
    );
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    wait_for_process_and_group_absence(child.id() as i32);
    assert_eq!(
        historical_fixture::observed_process_identity(child.id()),
        None,
        "reaped fixture must not retain a process identity"
    );
}

#[test]
fn increasing_capacity_preserves_both_historical_hold_classes_and_all_release_guards() {
    for managed in [false, true] {
        let mut f = Historical::new();
        if managed {
            f.managed_dispatch();
        }
        let root = f.store.root().to_path_buf();
        let modern = rows(&f.db());
        drop(f.store);
        assert!(
            luthor::state::capacity::change(
                &root,
                luthor::state::capacity::CapacityChangeRequest {
                    expected_capacity: 2,
                    expected_reservations: 1,
                    new_capacity: 3,
                    actor: "operator",
                    reason: "modern launches still require reconciliation",
                },
            )
            .is_err()
        );
        f.store = StateStore::open(&root, 2).unwrap();
        assert_eq!(rows(&f.db()), modern);
        f.predates_preparation();
        f.assert_hold();
        let before = rows(&f.db());
        drop(f.store);
        let change = luthor::state::capacity::change(
            &root,
            luthor::state::capacity::CapacityChangeRequest {
                expected_capacity: 2,
                expected_reservations: 1,
                new_capacity: 3,
                actor: "operator",
                reason: "count, never release, the historical hold",
            },
        )
        .unwrap();
        assert_eq!(change.permanent_holds.len(), 1);
        assert_eq!(change.permanent_holds[0].task_id, "task");
        assert_eq!(change.permanent_holds[0].attempt_id, "attempt-historical");
        f.store = StateStore::open(&root, 3).unwrap();
        // Only the queue-level audit sequence may change, never task history.
        let after = rows(&f.db());
        assert_eq!(&after[..6], &before[..6]);
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
        f.assert_hold();
        f.assert_hold();
    }
}
