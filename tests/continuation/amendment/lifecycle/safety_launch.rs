use luthor::state::{journal, scheduling};
mod ready;
mod setup;
use super::{Lane, database, gates};
use luthor::supervisor::{self, LaunchPlan};
use setup::{commit, drift, lane, script};
use std::{
    fs,
    io::Read,
    process::{Command, Stdio},
};

#[test]
fn safety_first_attempt_startup_refuses_late_tracked_untracked_and_descendant_drift() {
    for amended in [false, true] {
        for change in ["tracked", "untracked", "descendant"] {
            let mut lane = lane(amended);
            let plan = commit(&mut lane, amended);
            let path = lane
                .f
                .store
                .root()
                .join("attempts/attempt-task-a.plan.json");
            fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
            script::private(&path);
            drift(&plan, change);
            let output = Command::new(env!("CARGO_BIN_EXE_luthor"))
                .arg("__supervise")
                .arg(lane.f.store.root())
                .arg(&plan.attempt_id)
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert!(!output.status.success(), "{amended}: {change}");
            assert!(output.stdout.is_empty(), "no READY: {amended}: {change}");
            assert!(
                !lane
                    .f
                    .store
                    .root()
                    .join("attempts/attempt-task-a.child.json")
                    .exists()
            );
            assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
        }
    }
}

#[test]
fn safety_parent_ready_release_refuses_late_worktree_drift_without_sending_gate() {
    for amended in [false, true] {
        for change in ["tracked", "untracked", "descendant"] {
            let mut lane = lane(amended);
            let proxy = script::ready_proxy(&lane, change);
            let result = setup::launch(&mut lane, amended, &proxy);
            assert!(
                matches!(result, Err(supervisor::SupervisorError::Worktree(_))),
                "READY worktree check: {amended}: {change}: {result:?}"
            );
            assert_eq!(
                database(&lane)
                    .query_row(
                        "SELECT COUNT(*) FROM intents WHERE kind='gate_release'",
                        [],
                        |r| r.get::<_, usize>(0)
                    )
                    .unwrap(),
                0
            );
            script::await_proxy(&lane);
            assert!(
                fs::read(
                    lane.f
                        .store
                        .root()
                        .join("attempts/attempt-task-a.stdout.log")
                )
                .unwrap()
                .is_empty()
            );
            assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
        }
    }
}

struct RegisterGate<'a> {
    lane: &'a mut Lane,
}
impl Read for RegisterGate<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let root = self.lane.f.store.root();
        let registration =
            fs::read_to_string(root.join("attempts/attempt-task-a.child.json")).unwrap();
        let (boot, start) = gates::identity(std::process::id());
        let process = serde_json::json!({"pid":std::process::id(), "boot_identity":boot, "start_identity":start}).to_string();
        for (kind, payload) in [
            ("child_registered", registration.trim()),
            ("supervisor_ready", &process),
            ("gate_sent", &process),
        ] {
            journal::record_evidence(
                &mut self.lane.f.store,
                "task-a",
                Some("attempt-task-a"),
                kind,
                payload,
            )
            .unwrap();
        }
        journal::record_intent(
            &mut self.lane.f.store,
            "gate-attempt-task-a",
            "task-a",
            Some("attempt-task-a"),
            "gate_release",
            &process,
        )
        .unwrap();
        bytes[0] = b'R';
        Ok(1)
    }
}

#[test]
fn safety_real_worker_preexec_rechecks_late_drift_after_registered_gate_release() {
    for amended in [false, true] {
        for change in ["clean", "tracked", "untracked", "descendant"] {
            let mut lane = lane(amended);
            let plan: LaunchPlan = commit(&mut lane, amended);
            let shim = script::worker_shim(&lane, change);
            let attempts = lane.f.store.root().join("attempts");
            let owner = luthor::WorktreeOwner::acquire(lane.f.store.root(), &plan.task_id).unwrap();
            let status = supervisor::run_gated_child_with_binary(
                &plan,
                RegisterGate { lane: &mut lane },
                &attempts,
                &shim,
                &owner,
            )
            .unwrap();
            let stdout = fs::read(attempts.join("attempt-task-a.stdout.log")).unwrap();
            if change == "clean" {
                assert_eq!(
                    status.code(),
                    Some(17),
                    "valid registration must exec: {amended}"
                );
                assert!(!stdout.is_empty());
            } else {
                assert_ne!(status.code(), Some(17), "{amended}: {change}");
                assert!(
                    stdout.is_empty(),
                    "worker executable never ran: {amended}: {change}"
                );
            }
            assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
            assert_eq!(
                database(&lane)
                    .query_row("SELECT lifecycle FROM attempts", [], |r| r
                        .get::<_, String>(0))
                    .unwrap(),
                "launch_intended"
            );
        }
    }
}
