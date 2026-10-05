use super::*;
use luthor::state::{journal, scheduling, task_records, worktree_records};

#[cfg(unix)]
pub(crate) fn natural_exit_stale_assignee_is_held_despite_matching_open_pr() {
    let (_dir, mut config, mut store) = supervisor_support::stop_views::completion_fixture();
    config.capacity = 1;
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let identity = worktree_records::worktree_record(&store, "task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url.clone(), identity.branch)),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(selection.candidate, 0);
    let result = luthor::coordinator::reconcile_with_pr(
        &mut store,
        "task",
        "attempt-real",
        &mut projects,
        &mut prs,
    )
    .unwrap();
    supervisor_support::stop_views::assert_claim_hold(result);
    assert!(
        task_records::held_reason(&store, "task")
            .unwrap()
            .unwrap()
            .contains("completion claim changed")
    );
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "verified_open_pr")
    );
    assert!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let outcome: Option<String> = connection
        .query_row(
            "SELECT outcome FROM attempts WHERE id='attempt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome.as_deref(), Some("exit_code=Some(7);signal=None"));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert!(scheduling::ensure_dispatch_capacity(&store).is_err());
    assert_eq!(prs.reads, 1);
    assert_eq!(projects.1, 1);
}

#[cfg(unix)]
pub(crate) fn stopped_exit_stale_assignee_is_held_despite_matching_open_pr() {
    let (_dir, config, mut store) = supervisor_support::stop_views::completion_fixture();
    journal::record_stop_intent(&mut store, "task", "attempt-real").unwrap();
    edit_receipt(&config, |receipt| {
        receipt.stop_signals = vec![libc::SIGTERM]
    });
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let identity = worktree_records::worktree_record(&store, "task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url.clone(), identity.branch)),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(selection.candidate, 0);
    let result = luthor::coordinator::reconcile_with_pr(
        &mut store,
        "task",
        "attempt-real",
        &mut projects,
        &mut prs,
    )
    .unwrap();
    supervisor_support::stop_views::assert_claim_hold(result);
    assert!(
        task_records::held_reason(&store, "task")
            .unwrap()
            .unwrap()
            .contains("completion claim changed")
    );
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "verified_open_pr")
    );
    assert!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let outcome: Option<String> = connection
        .query_row(
            "SELECT outcome FROM attempts WHERE id='attempt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome.as_deref(), Some("exit_code=Some(7);signal=None"));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert!(scheduling::ensure_dispatch_capacity(&store).is_err());
    assert_eq!(prs.reads, 1);
    assert_eq!(projects.1, 1);
}
