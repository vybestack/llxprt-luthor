use super::*;
#[test]
fn native_continuation_missing_or_contradictory_predecessor_proof_never_reserves() {
    for mutation in [
        "receipt",
        "duplicate_exit",
        "worktree",
        "config",
        "prompt",
        "descendant",
        "forged_second",
        "dispatch",
    ] {
        let mut lane = fixture(false);
        match mutation {
            "receipt" => fs::remove_file(
                lane.f
                    .store
                    .root()
                    .join("attempts/attempt-task-a.receipt.json"),
            )
            .unwrap(),
            "duplicate_exit" => {
                database(&lane).execute_batch("INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE kind='attempt_exit'").unwrap();
            }
            "worktree" => {
                crate::git(
                    &lane.plan.worktree,
                    &["symbolic-ref", "HEAD", "refs/heads/changed"],
                );
            }
            "config" => lane
                .f
                .config
                .resume
                .args
                .push("--branch=luthor/task-a".into()),
            "prompt" => lane
                .f
                .config
                .resume
                .args
                .last_mut()
                .unwrap()
                .push_str(" drift"),
            "descendant" => {
                let (boot, start) = super::super::gates::identity(std::process::id());
                database(&lane).execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-a','attempt-task-a','tracked_descendant',?1)",[serde_json::json!({"pid":std::process::id(),"boot_identity":boot,"start_identity":start}).to_string()]).unwrap();
            }
            "forged_second" => {
                database(&lane).execute_batch("INSERT INTO attempts(id,task_id,lifecycle) VALUES('forged','task-a','launch_intended')").unwrap();
            }
            "dispatch" => {
                database(&lane).execute_batch("UPDATE intents SET detail=json_set(detail,'$.amendment_sequence',0) WHERE kind='supervisor_dispatch'").unwrap();
            }
            _ => unreachable!(),
        }
        let mut launcher = Launcher {
            count: 0,
            execute: true,
        };
        assert!(
            run(
                &mut lane,
                Mode::NaturalExitRetry,
                "attempt-task-a",
                "attempt-second",
                "ok",
                &mut launcher
            )
            .is_err(),
            "{mutation}"
        );
        assert_eq!(launcher.count, 0, "{mutation}");
        assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
    }
}

#[test]
fn native_continuation_accepts_reused_task_scoped_pid_without_ignoring_new_attempt_proof() {
    let mut lane = fixture(false);
    let (boot, _) = super::super::gates::identity(std::process::id());
    let registration = serde_json::json!({"pid":std::process::id(),"boot_identity":boot,"start_identity":"previous-start"}).to_string();
    database(&lane).execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-a',NULL,'tracked_descendant',?1)", [&registration]).unwrap();
    let mut launcher = Launcher {
        count: 0,
        execute: true,
    };
    run(
        &mut lane,
        Mode::NaturalExitRetry,
        "attempt-task-a",
        "attempt-second",
        "ok",
        &mut launcher,
    )
    .unwrap();
    assert_eq!(launcher.count, 1);
    let retained: String = database(&lane).query_row("SELECT payload FROM evidence WHERE task_id='task-a' AND attempt_id IS NULL AND kind='tracked_descendant'", [], |row| row.get(0)).unwrap();
    assert_eq!(retained, registration);
    settle(&mut lane, "attempt-second");
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
}
