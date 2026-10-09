use super::*;
use luthor::state::{journal, scheduling, task_records, worktree_records};

#[cfg(unix)]
pub(crate) fn operator_recovery_release_failure_rolls_back_for_absent_and_matching_pr() {
    for matching_pr in [false, true] {
        let (_dir, config, mut store) = dispatched_fixture(7);
        let payload =
            journal::evidence_payloads(&store, "task", "attempt-real", "supervisor_ready")
                .unwrap()
                .remove(0);
        let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
            .as_u64()
            .unwrap() as libc::pid_t;
        wait_for_process_and_group_absence(pid);
        fs::remove_file(receipt_path(&config)).unwrap();

        let selection = task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap();
        let branch = worktree_records::worktree_record(&store, "task")
            .unwrap()
            .unwrap()
            .identity
            .unwrap()
            .branch;
        let mut projects = OtherProject(selection.candidate.clone(), 1);
        let mut prs = ExitPr {
            matching: matching_pr.then_some((selection.candidate.issue_url, branch)),
            ..ExitPr::default()
        };
        let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER reject_release BEFORE UPDATE OF status ON reservations \
             WHEN NEW.status='released' AND OLD.status='reserved' \
             BEGIN SELECT RAISE(ABORT,'injected release failure'); END;",
        )
        .unwrap();

        let result = operator_recover_missing_receipt(
            &mut store,
            "task",
            "attempt-real",
            "operator",
            "receipt lost",
            &mut projects,
            &mut prs,
        );
        assert!(result.is_err(), "release trigger must abort recovery");
        assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
        assert_eq!(
            task_records::task_phase(&store, "task").unwrap().as_deref(),
            Some("held")
        );
        let kinds = journal::evidence_kinds(&store, "task").unwrap();
        for kind in ["telemetry_lost", "exit_pr_lookup", "verified_open_pr"] {
            assert!(!kinds.iter().any(|seen| seen == kind), "unexpected {kind}");
        }
        assert!(!kinds.iter().any(|kind| kind == "attempt_exit"));
        let (lifecycle, outcome): (String, Option<String>) = db
            .query_row(
                "SELECT lifecycle, outcome FROM attempts WHERE task_id='task' AND id='attempt-real'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(lifecycle, "launch_intended");
        assert_eq!(outcome, None);
        drop(db);
        drop(store);

        assert_recovery_rollback_after_restart(&config);
    }
}

#[cfg(unix)]
fn assert_recovery_rollback_after_restart(config: &Config) {
    let reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(scheduling::reservation_count(&reopened).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&reopened, "task")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert!(
        journal::evidence_kinds(&reopened, "task")
            .unwrap()
            .iter()
            .all(|kind| {
                ![
                    "telemetry_lost",
                    "exit_pr_lookup",
                    "verified_open_pr",
                    "attempt_exit",
                ]
                .contains(&kind.as_str())
            })
    );
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let (lifecycle, outcome): (String, Option<String>) = db
        .query_row(
            "SELECT lifecycle, outcome FROM attempts WHERE task_id='task' AND id='attempt-real'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(lifecycle, "launch_intended");
    assert_eq!(outcome, None);
}
