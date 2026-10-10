use super::{Lane, await_receipt, database, executable_lane, launch};
use luthor::WorktreeOwner;
use luthor::{
    state::{launches, scheduling, verify_amended_observation_plan},
    supervisor::LaunchPlan,
};
use rusqlite::Connection;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

struct LiveLane {
    lane: Lane,
    release: PathBuf,
    config: PathBuf,
}

impl LiveLane {
    fn new() -> Self {
        let lane = executable_lane();
        let release = lane.f.dir.path().join("worker-release");
        let config = lane.f.dir.path().join("operator-config.json");
        fs::write(&config, serde_json::to_vec(&lane.f.config).unwrap()).unwrap();
        let started = lane.f.dir.path().join("worker-started");
        fs::write(
            &lane.plan.executable,
            format!(
                "#!/bin/sh\nprintf '%s\\000' \"$@\"\ntouch '{}'\nwhile [ ! -f '{}' ]; do /bin/sleep 0.1; done\nexit 17\n",
                started.display(), release.display()
            ),
        ).unwrap();
        let mut live = Self {
            lane,
            release,
            config,
        };
        launch(&mut live.lane, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !started.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(started.exists(), "amended worker did not pass its gate");
        assert!(
            !live
                .lane
                .f
                .store
                .root()
                .join("attempts/attempt-task-a.receipt.json")
                .exists()
        );
        live
    }

    fn plan(&self) -> LaunchPlan {
        serde_json::from_slice(
            &fs::read(
                self.lane
                    .f
                    .store
                    .root()
                    .join("attempts/attempt-task-a.plan.json"),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn view(&self, command: &str) -> Value {
        let mut cli = Command::new(env!("CARGO_BIN_EXE_luthor"));
        cli.arg(command);
        if command == "show" {
            cli.arg("task-a");
        }
        let output = cli.arg("--config").arg(&self.config).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!result.to_string().contains("Work on "));
        if command == "status" {
            assert_eq!(result["capacity"]["reserved"], 1);
            result["tasks"][0].clone()
        } else {
            result
        }
    }

    fn assert_views(&self, phase: &str) {
        let status = self.view("status");
        let shown = self.view("show");
        for view in [&status, &shown] {
            assert_eq!(view["phase"], phase);
            assert_eq!(view["reserved_slot"], true);
            if phase == "running" {
                assert_eq!(view["process"]["attempt_id"], "attempt-task-a");
                for who in ["child", "supervisor"] {
                    assert!(view["process"][who]["pid"].as_u64().unwrap() > 0);
                    assert!(view["process"][who]["group_id"].as_u64().unwrap() > 0);
                    assert!(
                        !view["process"][who]["boot_identity"]
                            .as_str()
                            .unwrap()
                            .is_empty()
                    );
                    assert!(
                        !view["process"][who]["start_identity"]
                            .as_str()
                            .unwrap()
                            .is_empty()
                    );
                }
            } else {
                assert!(view["process"].is_null());
            }
        }
        let lifecycle = if phase == "running" {
            "running".to_owned()
        } else {
            database(&self.lane)
                .query_row("SELECT lifecycle FROM attempts", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap()
        };
        assert_eq!(status["latest_attempt_lifecycle"], lifecycle);
        assert_eq!(shown["attempts"][0]["lifecycle"], lifecycle);
        assert_eq!(status["process"], shown["process"]);
    }
}

impl Drop for LiveLane {
    fn drop(&mut self) {
        fs::write(&self.release, "release").unwrap();
        let receipt = self
            .lane
            .f
            .store
            .root()
            .join("attempts/attempt-task-a.receipt.json");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !receipt.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn live_amended_worker_is_running_in_real_cli_without_authorizing_another_launch() {
    let live = LiveLane::new();
    let plan = live.plan();
    let original = launches::launch_intent(&live.lane.f.store, "attempt-task-a")
        .unwrap()
        .unwrap();
    assert_ne!(serde_json::from_str::<LaunchPlan>(&original).unwrap(), plan);
    verify_amended_observation_plan(&database(&live.lane), live.lane.f.store.root(), &plan)
        .unwrap();
    live.assert_views("running");
    assert!(
        live.lane
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .is_err()
    );
    match WorktreeOwner::acquire(live.lane.f.store.root(), &plan.task_id) {
        Err(error) => assert_eq!(format!("{error:?}"), "Busy"),
        Ok(_) => panic!("second worktree acquisition succeeded while worker was alive"),
    }
    assert_eq!(
        launches::launch_intent(&live.lane.f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        original
    );
    assert_eq!(
        scheduling::reservation_count(&live.lane.f.store).unwrap(),
        1
    );
    fs::write(&live.release, "release").unwrap();
    await_receipt(&live.lane);
}

fn snapshot(db: &Connection) {
    db.execute_batch(
        "CREATE TEMP TABLE saved_intents AS SELECT * FROM intents;
        CREATE TEMP TABLE saved_evidence AS SELECT * FROM evidence;
        CREATE TEMP TABLE saved_attempts AS SELECT * FROM attempts;
        CREATE TEMP TABLE saved_reservations AS SELECT * FROM reservations;",
    )
    .unwrap();
}

fn restore(db: &Connection) {
    db.execute_batch(
        "BEGIN IMMEDIATE;
        DELETE FROM intents; INSERT INTO intents SELECT * FROM saved_intents;
        DELETE FROM evidence; INSERT INTO evidence SELECT * FROM saved_evidence;
        DELETE FROM reservations; DELETE FROM attempts;
        INSERT INTO attempts SELECT * FROM saved_attempts;
        INSERT INTO reservations SELECT * FROM saved_reservations;
        COMMIT;",
    )
    .unwrap();
}

#[test]
fn live_amended_cli_holds_missing_duplicate_tampered_and_raw_proofs() {
    let live = LiveLane::new();
    let db = database(&live.lane);
    snapshot(&db);
    for sql in [
        "DELETE FROM evidence WHERE kind='initial_branch_removed'",
        "DELETE FROM intents WHERE kind='initial_branch_removal_seal'",
        "DELETE FROM intents WHERE kind='supervisor_dispatch'",
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE kind='initial_branch_removed'",
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) SELECT 'duplicate',task_id,attempt_id,kind,detail FROM intents WHERE kind='initial_branch_removal_seal'",
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) SELECT 'duplicate',task_id,attempt_id,kind,detail FROM intents WHERE kind='supervisor_dispatch'",
        "UPDATE evidence SET payload=json_set(payload,'$.effective_plan.args[0]','forged') WHERE kind='initial_branch_removed'",
        "UPDATE intents SET detail=json_set(detail,'$.audit_payload','forged') WHERE kind='initial_branch_removal_seal'",
        "UPDATE intents SET detail=json_set(detail,'$.amendment_sequence',99999) WHERE kind='supervisor_dispatch'",
        "UPDATE intents SET detail=json_extract(detail,'$.effective_plan') WHERE kind='supervisor_dispatch'",
        "DELETE FROM evidence WHERE kind='initial_branch_removed'; DELETE FROM intents WHERE kind='initial_branch_removal_seal'; UPDATE intents SET detail=json_extract(detail,'$.effective_plan') WHERE kind='supervisor_dispatch'",
        "DELETE FROM evidence WHERE kind='gate_sent'",
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE kind='gate_sent'",
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) SELECT 'duplicate',task_id,attempt_id,kind,detail FROM intents WHERE kind='gate_release'",
        "UPDATE evidence SET payload=json_set(payload,'$.start_identity','forged') WHERE kind='supervisor_ready'",
        "UPDATE intents SET detail=json_set(detail,'$.args[0]','forged') WHERE kind='launch'",
        "UPDATE attempts SET lifecycle='completed'",
        "UPDATE attempts SET outcome='invented'",
    ] {
        db.execute_batch(sql).unwrap();
        live.assert_views("held");
        restore(&db);
    }
    live.assert_views("running");
}

#[test]
fn live_amended_cli_requires_private_matching_artifacts_and_os_identity() {
    use std::os::unix::fs::PermissionsExt;
    let live = LiveLane::new();
    let dir = live.lane.f.store.root().join("attempts");
    let plan_path = dir.join("attempt-task-a.plan.json");
    let plan = fs::read(&plan_path).unwrap();
    fs::set_permissions(&plan_path, fs::Permissions::from_mode(0o644)).unwrap();
    live.assert_views("held");
    fs::set_permissions(&plan_path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut forged: Value = serde_json::from_slice(&plan).unwrap();
    forged["args"][0] = Value::from("forged");
    fs::write(&plan_path, forged.to_string()).unwrap();
    live.assert_views("held");
    fs::write(&plan_path, plan).unwrap();
    let child_path = dir.join("attempt-task-a.child.json");
    let child = fs::read(&child_path).unwrap();
    let db = database(&live.lane);
    snapshot(&db);
    let mut forged: Value = serde_json::from_slice(&child).unwrap();
    forged["start_identity"] = Value::from("forged");
    fs::write(&child_path, forged.to_string()).unwrap();
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE kind='child_registered'",
        [forged.to_string()],
    )
    .unwrap();
    live.assert_views("held");
    restore(&db);
    fs::write(child_path, child).unwrap();
    live.assert_views("running");
}
