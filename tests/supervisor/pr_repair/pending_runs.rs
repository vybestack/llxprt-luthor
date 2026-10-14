//! Issue 75: failed jobs are pending until the containing run completes.
use super::{configured_checks::configure_gh, *};
use luthor::config::RequiredChecks;
use serde_json::{Value, json};

fn fixture() -> Fixture {
    let mut f = Fixture::with_checks(RequiredChecks::Names(vec!["quality gates".into()]));
    configure_gh(&mut f);
    f
}

fn run_evidence(f: &Fixture, status: &str, head: &str) {
    let gh = &f.prs.gh_health.as_ref().unwrap().executable;
    fs::write(
        gh.with_extension("run"),
        json!({"id":123,"head_sha":head,"repository":{"full_name":"org/code"},"status":status})
            .to_string(),
    )
    .unwrap();
}

fn posts(f: &Fixture) -> usize {
    let gh = &f.prs.gh_health.as_ref().unwrap().executable;
    fs::read_to_string(gh.with_extension("calls"))
        .unwrap()
        .lines()
        .filter(|line| line.contains("--method POST"))
        .count()
}

fn evidence(f: &Fixture, kind: &str) -> Vec<String> {
    journal::evidence_payloads(&f.store, "task", "attempt-real", kind).unwrap()
}

fn assert_waiting(f: &mut Fixture) {
    // The completed initial worker can have historical hold evidence; waiting
    // must not add a new reason or transition the task back to held.
    let reason_before = task_records::held_reason(&f.store, "task").unwrap();
    let holds_before = evidence(f, "held_reason");
    let outcome = f.repair_outcome();
    assert!(outcome.held.is_empty(), "{:?}", outcome.held);
    assert!(outcome.launched.is_empty());
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    assert_eq!(
        task_records::held_reason(&f.store, "task").unwrap(),
        reason_before
    );
    assert_eq!(evidence(f, "held_reason"), holds_before);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}

fn pending_release_fixture() -> Fixture {
    let mut f = Fixture::with_checks(RequiredChecks::Names(vec![
        "quality gates".into(),
        "release build".into(),
    ]));
    configure_gh(&mut f);
    // Include another required check that is still running, as in #75.
    let gh = &f.prs.gh_health.as_ref().unwrap().executable;
    let mut health: Value =
        serde_json::from_slice(&fs::read(gh.with_extension("health")).unwrap()).unwrap();
    let nodes = health
        .pointer_mut(
            "/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup/contexts/nodes",
        )
        .unwrap()
        .as_array_mut()
        .unwrap();
    nodes[1]["name"] = json!("release build");
    nodes[1]["isRequired"] = json!(true);
    nodes[1]["status"] = json!("IN_PROGRESS");
    nodes[1]["conclusion"] = Value::Null;
    fs::write(gh.with_extension("health"), health.to_string()).unwrap();
    f
}

#[test]
fn unfinished_failed_run_waits_without_evidence_then_reruns_exactly_once() {
    for status in ["in_progress", "queued"] {
        let mut f = pending_release_fixture();
        run_evidence(&f, status, &f.prs.head);
        for _ in 0..2 {
            assert_waiting(&mut f);
            assert_eq!(posts(&f), 0);
            for kind in [
                "pr_failed_jobs_rerun_intent",
                "pr_failed_jobs_rerun",
                "pr_failed_jobs_rerun_acknowledged",
            ] {
                assert!(evidence(&f, kind).is_empty(), "{status}: {kind}");
            }
        }
        let observed: Value =
            serde_json::from_str(evidence(&f, "pr_health").last().unwrap()).unwrap();
        assert_eq!(observed["required_checks_pending"], true);
        drop(f.store);
        f.store = StateStore::open(&f.config.state_root, 1).unwrap();
        run_evidence(&f, "completed", &f.prs.head);
        assert_waiting(&mut f);
        assert_eq!(posts(&f), 1);
        let reruns = evidence(&f, "pr_failed_jobs_rerun");
        assert_eq!(reruns.len(), 1);
        assert_eq!(evidence(&f, "pr_failed_jobs_rerun_intent"), reruns);
        assert_eq!(evidence(&f, "pr_failed_jobs_rerun_acknowledged"), reruns);
        let audit: Value = serde_json::from_str(&reruns[0]).unwrap();
        assert_eq!(audit["head"], f.prs.head);
        assert_eq!(audit["run_id"], 123);
        assert_eq!(audit["run_attempt"], 1);
        assert_eq!(audit["checks"], json!(["quality gates"]));
        drop(f.store);
        f.store = StateStore::open(&f.config.state_root, 1).unwrap();
        assert_waiting(&mut f);
        assert_eq!(posts(&f), 1);
        assert_eq!(evidence(&f, "pr_failed_jobs_rerun"), reruns);
    }
}

#[test]
fn pending_failed_run_still_holds_on_exact_head_mismatch() {
    for status in ["in_progress", "queued", "completed"] {
        let mut f = fixture();
        run_evidence(&f, status, "other-head");
        let outcome = f.repair_outcome();
        assert_eq!(outcome.held.len(), 1);
        assert!(outcome.launched.is_empty());
        assert!(
            outcome.held[0]
                .reason
                .contains("Malformed, invalid-failed-run")
        );
        assert_eq!(
            task_records::task_phase(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("held")
        );
        assert_eq!(posts(&f), 0);
        assert!(evidence(&f, "pr_failed_jobs_rerun_intent").is_empty());
        assert!(evidence(&f, "pr_failed_jobs_rerun").is_empty());
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    }
}

#[test]
fn rejected_rerun_records_only_intent_and_never_repeats_post() {
    let mut f = fixture();
    let gh = &f.prs.gh_health.as_ref().unwrap().executable;
    let script = fs::read_to_string(gh).unwrap();
    let accepted = "'api --method POST repos/org/code/actions/runs/123/rerun-failed-jobs') exit 0";
    assert!(script.contains(accepted));
    fs::write(
        gh,
        script.replace(accepted, &accepted.replace("exit 0", "exit 1")),
    )
    .unwrap();
    let outcome = f.repair_outcome();
    assert_eq!(outcome.held.len(), 1);
    assert!(outcome.launched.is_empty());
    assert_eq!(posts(&f), 1);
    assert_eq!(evidence(&f, "pr_failed_jobs_rerun_intent").len(), 1);
    assert!(evidence(&f, "pr_failed_jobs_rerun").is_empty());
    assert!(evidence(&f, "pr_failed_jobs_rerun_acknowledged").is_empty());
    // Even after a restart and operator release, ambiguous intent is a hold.
    drop(f.store);
    f.store = StateStore::open(&f.config.state_root, 1).unwrap();
    rusqlite::Connection::open(f.config.state_root.join("state.sqlite3"))
        .unwrap()
        .execute("UPDATE tasks SET state='pr_complete' WHERE id='task'", [])
        .unwrap();
    assert_eq!(f.repair_outcome().held.len(), 1);
    assert_eq!(posts(&f), 1);
    assert!(evidence(&f, "pr_failed_jobs_rerun").is_empty());
}

#[test]
fn interrupted_new_rerun_intent_holds_without_post() {
    let mut f = fixture();
    let payload =
        json!({"head":f.prs.head,"run_id":123,"run_attempt":1,"checks":["quality gates"]});
    journal::record_evidence(
        &mut f.store,
        "task",
        Some("attempt-real"),
        "pr_failed_jobs_rerun_intent",
        &payload.to_string(),
    )
    .unwrap();
    assert_eq!(f.repair_outcome().held.len(), 1);
    assert_eq!(posts(&f), 0);
    assert!(evidence(&f, "pr_failed_jobs_rerun").is_empty());
    assert!(evidence(&f, "pr_failed_jobs_rerun_acknowledged").is_empty());
}
