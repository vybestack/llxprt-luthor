use super::*;
use luthor::state::{journal, scheduling, task_records, worktree_records};

#[cfg(unix)]
pub(crate) fn operator_recovery_does_not_count_nonmatching_pr() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let payload = journal::evidence_payloads(&store, "task", "attempt-real", "supervisor_ready")
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
        matching: Some(("https://github.com/org/tracker/issues/999".into(), branch)),
        ..ExitPr::default()
    };
    assert_eq!(
        operator_recover_missing_receipt(
            &mut store,
            "task",
            "attempt-real",
            "operator",
            "receipt lost",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        luthor::coordinator::RecoveryResult::RecoveredHeld
    );
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .filter(|kind| *kind == "verified_open_pr")
            .count(),
        0
    );
}

#[cfg(unix)]
pub(crate) fn operator_recovery_rejects_wrong_pr_head_and_author() {
    for wrong_head in [true, false] {
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
            matching: Some((
                selection.candidate.issue_url,
                if wrong_head {
                    "wrong-branch".into()
                } else {
                    branch
                },
            )),
            author: (!wrong_head).then(|| "attacker".into()),
            ..ExitPr::default()
        };
        assert!(matches!(
            operator_recover_missing_receipt(
                &mut store,
                "task",
                "attempt-real",
                "operator",
                "receipt lost",
                &mut projects,
                &mut prs
            )
            .unwrap(),
            luthor::coordinator::RecoveryResult::Held(_)
        ));
        assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
        assert_eq!(
            task_records::task_phase(&store, "task").unwrap().as_deref(),
            Some("held")
        );
        let kinds = journal::evidence_kinds(&store, "task").unwrap();
        assert!(
            !kinds
                .iter()
                .any(|kind| kind == "telemetry_lost" || kind == "verified_open_pr")
        );
    }
}

#[cfg(unix)]
pub(crate) fn operator_recovery_stale_claim_keeps_slot_reserved_without_pr_lookup() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let payload = journal::evidence_payloads(&store, "task", "attempt-real", "supervisor_ready")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    wait_for_process_and_group_absence(pid);
    fs::remove_file(receipt_path(&config)).unwrap();
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let mut projects = OtherProject(selection.candidate, 0);
    let mut prs = ExitPr::default();
    assert!(matches!(
        operator_recover_missing_receipt(
            &mut store,
            "task",
            "attempt-real",
            "operator",
            "receipt lost",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        luthor::coordinator::RecoveryResult::Held(_)
    ));
    assert_eq!(prs.reads, 0);
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
}
