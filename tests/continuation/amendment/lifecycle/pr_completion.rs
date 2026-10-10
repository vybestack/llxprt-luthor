use super::observation::exited_lane;
use super::{Lane, database};
use luthor::state::{journal, scheduling, task_records};
use luthor::{
    coordinator::reconcile_with_pr,
    github::pull_request::{LookupError, PullRequestReader},
    state::{verify_amended_observation_plan, verify_amended_worker_plan},
    supervisor::{self, LaunchPlan, Reconciliation},
};
use serde_json::{Value, json};

pub(super) struct PrReader {
    pub(super) inner: crate::FakePr,
    pub(super) mutation: &'static str,
}
impl PullRequestReader for PrReader {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        if self.mutation == "login" {
            Ok("other".into())
        } else {
            self.inner.authenticated_identity()
        }
    }
    fn page(&mut self, repo: &str, page: u32) -> Result<Vec<Value>, LookupError> {
        self.inner.page(repo, page)
    }
    fn repository_identity(&mut self, repo: &str) -> Result<u64, LookupError> {
        self.inner.repository_identity(repo)
    }
    fn detail(&mut self, repo: &str, number: u64) -> Result<Value, LookupError> {
        let mut detail = self.inner.detail(repo, number)?;
        detail["head"]["ref"] = json!("luthor/task-a");
        match self.mutation {
            "branch" => detail["head"]["ref"] = json!("wrong"),
            "author" => detail["user"]["login"] = json!("other"),
            "repository" => detail["base"]["repo"]["id"] = json!(99),
            _ => {}
        }
        Ok(detail)
    }
}

fn complete(mutation: &'static str) -> (Lane, LaunchPlan, Reconciliation) {
    complete_from(exited_lane(), mutation)
}

pub(super) fn complete_from(
    (mut lane, plan): (Lane, LaunchPlan),
    mutation: &'static str,
) -> (Lane, LaunchPlan, Reconciliation) {
    let mut prs = PrReader {
        inner: crate::FakePr {
            present_on: Some(1),
            ..Default::default()
        },
        mutation,
    };
    let result = reconcile_with_pr(
        &mut lane.f.store,
        "task-a",
        "attempt-task-a",
        &mut lane.github,
        &mut prs,
    )
    .unwrap();
    (lane, plan, result)
}

#[test]
fn amended_natural_exit_matching_pr_remains_valid_on_reconciliation_and_status_read() {
    let (mut lane, plan, result) = complete("matching");
    assert!(matches!(result, Reconciliation::Completed { .. }));
    assert_eq!(
        task_records::task_phase(&lane.f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).unwrap();
    assert!(matches!(
        supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap(),
        Reconciliation::Completed { .. }
    ));
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
    assert!(verify_amended_worker_plan(&database(&lane), lane.f.store.root(), &plan).is_err());
    assert!(
        lane.f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .is_err()
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(["show", "task-a", "--config"])
        .arg(write_config(&lane))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["phase"], "pr_complete");
}

fn write_config(lane: &Lane) -> std::path::PathBuf {
    let path = lane.f.dir.path().join("status-config.json");
    std::fs::write(&path, serde_json::to_vec(&lane.f.config).unwrap()).unwrap();
    path
}

#[test]
fn amended_pr_completion_rejects_wrong_authenticated_pr_and_keeps_held_terminal_attempt() {
    for mutation in ["login", "branch", "author", "repository"] {
        let (mut lane, plan, result) = complete(mutation);
        assert!(matches!(result, Reconciliation::Held { .. }), "{mutation}");
        assert_eq!(
            task_records::task_phase(&lane.f.store, "task-a")
                .unwrap()
                .as_deref(),
            Some("held")
        );
        verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).unwrap();
        assert!(matches!(
            supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap(),
            Reconciliation::Completed { .. }
        ));
        assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
    }
}

#[test]
fn amended_natural_exit_without_pr_keeps_attention_and_no_completion_binding() {
    let (mut lane, plan) = exited_lane();
    let mut prs = crate::FakePr::default();
    assert!(matches!(
        reconcile_with_pr(
            &mut lane.f.store,
            "task-a",
            "attempt-task-a",
            &mut lane.github,
            &mut prs
        )
        .unwrap(),
        Reconciliation::Completed { .. }
    ));
    assert_eq!(
        task_records::task_phase(&lane.f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attention")
    );
    verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).unwrap();
    let bindings: usize = database(&lane)
        .query_row(
            "SELECT COUNT(*) FROM intents WHERE kind='amended_pr_completion'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bindings, 0);
    assert!(
        journal::evidence_payloads(
            &lane.f.store,
            "task-a",
            "attempt-task-a",
            "verified_open_pr"
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
}

#[test]
fn amended_pr_completion_rejects_forged_duplicate_wrong_receipt_and_unauthorized_replay() {
    for sql in [
        "DELETE FROM intents WHERE kind='amended_pr_completion'",
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) SELECT 'duplicate',task_id,attempt_id,kind,detail FROM intents WHERE kind='amended_pr_completion'",
        "UPDATE intents SET attempt_id='foreign' WHERE kind='amended_pr_completion'",
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE kind='verified_open_pr'",
        "UPDATE evidence SET attempt_id='foreign' WHERE kind='verified_open_pr'",
        "UPDATE evidence SET payload=json_set(payload,'$.id',999) WHERE kind='verified_open_pr'",
        "UPDATE evidence SET payload=json_set(payload,'$.active_login','other') WHERE kind='verified_open_pr'",
        "UPDATE evidence SET payload=json_set(payload,'$.attempt_id','foreign') WHERE kind='verified_open_pr'",
        "UPDATE evidence SET payload=json_set(payload,'$.child_pid',999) WHERE kind='attempt_exit'",
        "DELETE FROM intents WHERE kind='gate_release'",
        "DELETE FROM evidence WHERE kind='gate_sent'",
        "UPDATE intents SET detail=json_set(detail,'$.amendment_sequence',999) WHERE kind='supervisor_dispatch'",
        "DELETE FROM evidence WHERE kind='initial_branch_removed'",
        "DELETE FROM evidence WHERE kind='exit_pr_lookup'",
        "UPDATE evidence SET payload=json_set(payload,'$.status.status','absent') WHERE kind='exit_pr_lookup'",
        "INSERT INTO attempts(id,task_id,lifecycle) VALUES('replay','task-a','launch_intended')",
        "UPDATE reservations SET status='reserved'",
    ] {
        let (mut lane, plan, _) = complete("matching");
        database(&lane).execute_batch(sql).unwrap();
        assert!(
            verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).is_err(),
            "{sql}"
        );
        assert!(
            matches!(
                supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a")
                    .unwrap(),
                Reconciliation::Held { .. }
            ),
            "{sql}"
        );
    }
}

#[test]
fn amended_completion_preserves_held_lookup_retry_path() {
    let (mut lane, plan) = exited_lane();
    let mut prs = PrReader {
        inner: crate::FakePr {
            fail_on: Some(1),
            ..Default::default()
        },
        mutation: "matching",
    };
    assert!(matches!(
        reconcile_with_pr(
            &mut lane.f.store,
            "task-a",
            "attempt-task-a",
            &mut lane.github,
            &mut prs
        )
        .unwrap(),
        Reconciliation::Held { .. }
    ));
    verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).unwrap();
    assert_eq!(
        task_records::task_phase(&lane.f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
    prs.inner.present_on = Some(2);
    assert!(matches!(
        reconcile_with_pr(
            &mut lane.f.store,
            "task-a",
            "attempt-task-a",
            &mut lane.github,
            &mut prs
        )
        .unwrap(),
        Reconciliation::Completed { .. }
    ));
    assert_eq!(
        task_records::task_phase(&lane.f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).unwrap();
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
}
