use super::{ContinuationResult, Lane, Refusal};
use luthor::{state::EffectiveConfigSnapshot, supervisor::LaunchPlan};
use rusqlite::{Connection, params};

#[test]
fn continuation_stale_context_and_failed_audit_never_reach_launcher() {
    let mut lane = Lane::new();
    lane.local.sql_on_recheck = Some(
        "INSERT INTO evidence(task_id,kind,payload) VALUES('task-a','held_reason','external observation');",
    );
    lane.held(Refusal::AuthorizationFailed);
    let mut lane = Lane::new();
    let db = Connection::open(lane.f.store.root().join("state.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER audit_failure BEFORE INSERT ON evidence WHEN NEW.kind='never_dispatched_authorized' BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    lane.held(Refusal::AuthorizationFailed);
    assert!(
        lane.f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .is_ok()
    );
}

#[test]
fn continuation_invalid_saved_argv_holds_without_amending_intent() {
    let mut lane = Lane::new();
    let mut plan = lane.plan.clone();
    plan.args.push("different-from-saved-template".into());
    let saved = serde_json::to_string_pretty(&plan).unwrap();
    let db = Connection::open(lane.f.store.root().join("state.sqlite3")).unwrap();
    db.execute("UPDATE intents SET detail=?1 WHERE kind='launch'", [&saved])
        .unwrap();
    lane.held(Refusal::PlanInvalid);
    assert_eq!(
        lane.f
            .store
            .launch_intent("attempt-task-a")
            .unwrap()
            .unwrap(),
        saved
    );
}

#[test]
fn continuation_preserves_full_legacy_template_and_exact_saved_argv() {
    let mut lane = Lane::new();
    lane.f
        .config
        .initial
        .args
        .extend(["--max-tool-calls".into(), "1024".into()]);
    lane.plan
        .args
        .extend(["--max-tool-calls".into(), "1024".into()]);
    let mut selection = lane.f.store.selection_evidence("task-a").unwrap().unwrap();
    selection.effective_config = EffectiveConfigSnapshot::from(&lane.f.config);
    let saved = serde_json::to_string_pretty(&lane.plan).unwrap();
    let db = Connection::open(lane.f.store.root().join("state.sqlite3")).unwrap();
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE kind='selection'",
        params![serde_json::to_string(&selection).unwrap()],
    )
    .unwrap();
    db.execute("UPDATE intents SET detail=?1 WHERE kind='launch'", [&saved])
        .unwrap();
    assert_eq!(
        lane.run(),
        ContinuationResult::Dispatched(Box::new(lane.plan.clone()))
    );
    assert_eq!(lane.launcher.plans, [lane.plan.clone()]);
    let persisted: LaunchPlan = serde_json::from_str(
        &lane
            .f
            .store
            .launch_intent("attempt-task-a")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(persisted, lane.plan);
    assert_eq!(
        lane.f
            .store
            .launch_intent("attempt-task-a")
            .unwrap()
            .unwrap(),
        saved
    );
}
