mod continuation;
mod refusals;
use super::{Lane, amend, database, fixture};
use luthor::state::{EffectiveConfigSnapshot, verify_amended_worker_plan};
pub(super) use refusals::reseal;
use serde_json::{Value, json};

pub(super) fn dual_fixture() -> Lane {
    let mut lane = fixture();
    lane.f.config.resume.executable = lane.f.config.initial.executable.clone();
    let mut selection = lane.f.store.selection_evidence("task-a").unwrap().unwrap();
    selection.effective_config.resume = lane.f.config.resume.clone();
    selection
        .effective_config
        .resume
        .args
        .splice(4..4, ["--branch".into(), "luthor/{task.id}".into()]);
    database(&lane)
        .execute(
            "UPDATE evidence SET payload=?1 WHERE kind='selection'",
            [serde_json::to_string(&selection).unwrap()],
        )
        .unwrap();
    lane
}

pub(super) fn audit_json(lane: &Lane) -> Value {
    let payload = lane
        .f
        .store
        .evidence_payloads("task-a", "attempt-task-a", "initial_branch_removed")
        .unwrap();
    assert_eq!(payload.len(), 1);
    serde_json::from_str(&payload[0]).unwrap()
}

fn original_rows(lane: &Lane) -> Vec<String> {
    let db = database(lane);
    db.prepare("SELECT 'e:' || sequence || ':' || payload FROM evidence ORDER BY sequence")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .chain(
            db.prepare("SELECT 'i:' || sequence || ':' || detail FROM intents ORDER BY sequence")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        )
        .collect()
}

#[test]
fn future_template_dual_correction_has_distinct_version_and_preserves_original_evidence() {
    let mut lane = dual_fixture();
    let before = original_rows(&lane);
    let original_selection = lane.f.store.selection_evidence("task-a").unwrap().unwrap();
    let original = lane
        .f
        .store
        .launch_intent("attempt-task-a")
        .unwrap()
        .unwrap();
    amend(&mut lane).unwrap();
    let audit = audit_json(&lane);
    assert_eq!(audit["schema_version"], 3);
    assert_eq!(
        audit["future_template_correction"],
        json!({
            "policy":"native_initial_and_resume_branch_removal_v1",
            "initial":{"index":2,"removed":["--branch","luthor/{task.id}"]},
            "resume":{"index":4,"removed":["--branch","luthor/{task.id}"]}
        })
    );
    assert_eq!(
        audit["delta"],
        json!({"index":2,"removed":["--branch","luthor/task-a"]})
    );
    assert_eq!(audit["current_config_revision"], "corrected-revision");
    assert_eq!(
        audit["current_config"],
        serde_json::to_value(EffectiveConfigSnapshot::from(&lane.f.config)).unwrap()
    );
    assert_eq!(
        lane.f.store.selection_evidence("task-a").unwrap().unwrap(),
        original_selection
    );
    assert_eq!(
        lane.f
            .store
            .launch_intent("attempt-task-a")
            .unwrap()
            .unwrap(),
        original
    );
    let after = original_rows(&lane);
    assert!(before.iter().all(|row| after.contains(row)));
    assert_eq!(after.len(), before.len() + 2);
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    let mut expected = lane.plan.clone();
    expected.args.drain(2..4);
    assert_eq!(context.effective_plan(), &expected);
    assert_eq!(
        context.effective_plan().config_revision,
        lane.plan.config_revision
    );
    assert_eq!(context.effective_plan().args.last(), lane.plan.args.last());
    assert!(lane.f.store.resume_context("task-a").is_err());
    assert!(
        luthor::state::retry_context_for_task(&lane.f.store, "task-a", "attempt-task-a").is_err()
    );
    let proof = lane
        .f
        .store
        .begin_amended_supervision(&context, &lane.f.config, "corrected-revision")
        .unwrap();
    verify_amended_worker_plan(&database(&lane), lane.f.store.root(), &proof.effective_plan)
        .unwrap();
    assert!(amend(&mut lane).is_err());
    drop(lane.f.store);
    lane.f.store = luthor::state::StateStore::open(&lane.f.config.state_root, 1).unwrap();
    verify_amended_worker_plan(&database(&lane), lane.f.store.root(), &proof.effective_plan)
        .unwrap();
}

#[test]
fn future_template_initial_only_remains_schema_two_with_resume_unchanged() {
    let mut lane = dual_fixture();
    lane.f
        .config
        .resume
        .args
        .splice(4..4, ["--branch".into(), "luthor/{task.id}".into()]);
    amend(&mut lane).unwrap();
    let audit = audit_json(&lane);
    assert_eq!(audit["schema_version"], 2);
    assert!(audit.get("future_template_correction").is_none());
    let saved = lane.f.store.selection_evidence("task-a").unwrap().unwrap();
    assert_eq!(
        audit["current_config"]["resume"],
        serde_json::to_value(saved.effective_config.resume).unwrap()
    );
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    lane.f
        .store
        .amended_dispatch_proof(&context, &lane.f.config, "corrected-revision")
        .unwrap();
}
