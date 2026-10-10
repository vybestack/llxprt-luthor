use super::{ContinuationResult, Lane, Refusal};
use luthor::coordinator::{
    ContinuationDependencies, OsContinuationLocalInspector, continue_never_dispatched,
};
use luthor::state::{journal, scheduling, task_records};
use rusqlite::Connection;
use std::fs;

#[test]
#[cfg(unix)]
fn continuation_storage_symlinks_and_wrong_type_are_not_repaired() {
    use std::os::unix::fs::symlink;
    for variant in ["symlink", "broken", "file"] {
        let mut lane = Lane::new();
        let attempts = lane.f.store.root().join("attempts");
        fs::remove_dir(&attempts).unwrap();
        if variant == "file" {
            fs::write(&attempts, "wrong type").unwrap();
        } else {
            symlink(
                if variant == "broken" {
                    lane.f.dir.path().join("missing")
                } else {
                    lane.f.dir.path().to_owned()
                },
                &attempts,
            )
            .unwrap();
        }
        lane.held(Refusal::StorageUnavailable);
        assert!(fs::symlink_metadata(&attempts).is_ok());
    }
}

#[test]
fn continuation_missing_storage_is_only_created_by_launcher_not_inspection() {
    let mut lane = Lane::new();
    let attempts = lane.f.store.root().join("attempts");
    fs::remove_dir(&attempts).unwrap();
    assert_eq!(
        lane.run(),
        ContinuationResult::Dispatched(Box::new(lane.plan.clone()))
    );
    assert!(
        !attempts.exists(),
        "fake launcher and read-only inspection never create private files"
    );
}

#[test]
fn continuation_unexecutable_saved_worker_holds_before_authorization() {
    let mut lane = Lane::new();
    lane.plan.executable = lane.f.dir.path().join("missing-worker");
    lane.f.config.initial.executable = lane.plan.executable.clone();
    let mut selection = task_records::selection_evidence(&lane.f.store, "task-a")
        .unwrap()
        .unwrap();
    selection.effective_config.initial.executable = lane.plan.executable.clone();
    let db = Connection::open(lane.f.store.root().join("state.sqlite3")).unwrap();
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE kind='selection'",
        [serde_json::to_string(&selection).unwrap()],
    )
    .unwrap();
    db.execute(
        "UPDATE intents SET detail=?1 WHERE kind='launch'",
        [serde_json::to_string(&lane.plan).unwrap()],
    )
    .unwrap();
    let mut prs = std::mem::take(&mut lane.github.prs);
    let result = continue_never_dispatched(
        &mut lane.f.store,
        ContinuationDependencies {
            task_id: "task-a",
            attempt_id: "attempt-task-a",
            actor: "bot",
            config: &lane.f.config,
            config_revision: "revision",
            projects: &mut lane.github,
            prs: &mut prs,
            launcher: &mut lane.launcher,
            local: &mut OsContinuationLocalInspector,
            processes: &mut lane.processes,
        },
    )
    .unwrap();
    assert_eq!(
        result,
        ContinuationResult::Held(Refusal::ExecutableUnavailable)
    );
    assert!(lane.launcher.plans.is_empty());
    assert!(
        journal::evidence_payloads(
            &lane.f.store,
            "task-a",
            "attempt-task-a",
            "never_dispatched_authorized"
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
}

#[test]
fn continuation_pr_absence_requires_exhaustive_pages() {
    let mut lane = Lane::new();
    lane.read_scenario = "paged_absent";
    assert_eq!(
        lane.run(),
        ContinuationResult::Dispatched(Box::new(lane.plan.clone()))
    );
    assert_eq!(lane.github.prs.lookups, 2);
    let mut lane = Lane::new();
    lane.read_scenario = "paged_present";
    lane.github.prs.present_on = Some(2);
    lane.held(Refusal::PrPresent);
    assert_eq!(lane.github.prs.lookups, 2);
    let mut lane = Lane::new();
    lane.read_scenario = "paged_error";
    lane.github.prs.fail_on = Some(2);
    lane.held(Refusal::PrUnavailable);
    assert_eq!(lane.github.prs.lookups, 2);
}
