use super::{amend, audit_json, database, dual_fixture};
use serde_json::Value;

#[test]
fn future_template_initial_only_cannot_be_relabelled_as_dual() {
    let mut dual = super::dual_fixture();
    super::amend(&mut dual).unwrap();
    let correction = super::audit_json(&dual)["future_template_correction"].clone();
    let mut single = super::dual_fixture();
    single
        .f
        .config
        .resume
        .args
        .splice(4..4, ["--branch".into(), "luthor/{task.id}".into()]);
    super::amend(&mut single).unwrap();
    let mut audit = super::audit_json(&single);
    audit["schema_version"] = 3.into();
    audit["future_template_correction"] = correction;
    reseal(&single, &audit);
    assert!(
        single
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .is_err()
    );
}

#[test]
fn future_template_dual_rejects_every_unrelated_config_change() {
    for mutation in [
        "initial_args",
        "resume_args",
        "initial_executable",
        "resume_executable",
        "state_root",
        "worktree_root",
        "mapping",
        "source",
        "login",
        "capacity",
    ] {
        let mut lane = dual_fixture();
        match mutation {
            "initial_args" => lane.f.config.initial.args.push("extra".into()),
            "resume_args" => lane.f.config.resume.args.push("extra".into()),
            "initial_executable" => lane.f.config.initial.executable = "/other".into(),
            "resume_executable" => lane.f.config.resume.executable = "/other".into(),
            "state_root" => lane.f.config.state_root = "/other".into(),
            "worktree_root" => lane.f.config.worktree_root = "/other".into(),
            "mapping" => lane.f.config.mappings[0].push_remote = "other".into(),
            "source" => lane.f.config.sources[0].milestone = None,
            "login" => lane.f.config.assignment_login = "other".into(),
            "capacity" => lane.f.config.capacity = 2,
            _ => unreachable!(),
        }
        assert!(amend(&mut lane).is_err(), "{mutation}");
        assert!(
            lane.f
                .store
                .evidence_payloads("task-a", "attempt-task-a", "initial_branch_removed")
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn future_template_dual_requires_exact_unique_adjacent_resume_pair() {
    for mutation in [
        "duplicate",
        "inline",
        "foreign",
        "split",
        "missing",
        "not_native",
    ] {
        let mut lane = dual_fixture();
        let mut selection = lane.f.store.selection_evidence("task-a").unwrap().unwrap();
        let resume = &mut selection.effective_config.resume;
        match mutation {
            "duplicate" => resume
                .args
                .extend(["--branch".into(), "luthor/{task.id}".into()]),
            "inline" => {
                resume
                    .args
                    .splice(4..6, ["--branch=luthor/{task.id}".into()]);
            }
            "foreign" => resume.args[5] = "luthor/other".into(),
            "split" => resume.args.insert(5, "--other".into()),
            "missing" => {
                resume.args.remove(4);
            }
            "not_native" => {
                resume.executable = "/bin/worker".into();
                lane.f.config.resume.executable = resume.executable.clone();
            }
            _ => unreachable!(),
        }
        database(&lane)
            .execute(
                "UPDATE evidence SET payload=?1 WHERE kind='selection'",
                [serde_json::to_string(&selection).unwrap()],
            )
            .unwrap();
        assert!(amend(&mut lane).is_err(), "{mutation}");
    }
}

pub(crate) fn reseal(lane: &super::Lane, audit: &Value) {
    let db = database(lane);
    let payload = serde_json::to_string(audit).unwrap();
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE kind='initial_branch_removed'",
        [&payload],
    )
    .unwrap();
    db.execute("UPDATE intents SET detail=json_set(detail,'$.audit_payload',?1) WHERE kind='initial_branch_removal_seal'", [&payload]).unwrap();
}

#[test]
fn future_template_dual_rejects_relabeled_policy_and_delta_even_with_matching_seal() {
    for mutation in [
        "v2",
        "v4",
        "missing",
        "policy",
        "initial_index",
        "resume_index",
        "resume_value",
        "config",
        "plan",
        "unknown",
    ] {
        let mut lane = dual_fixture();
        amend(&mut lane).unwrap();
        let context = lane
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap();
        let mut audit = audit_json(&lane);
        match mutation {
            "v2" => audit["schema_version"] = 2.into(),
            "v4" => audit["schema_version"] = 4.into(),
            "missing" => {
                audit
                    .as_object_mut()
                    .unwrap()
                    .remove("future_template_correction");
            }
            "policy" => audit["future_template_correction"]["policy"] = "generic_snapshot".into(),
            "initial_index" => audit["future_template_correction"]["initial"]["index"] = 0.into(),
            "resume_index" => audit["future_template_correction"]["resume"]["index"] = 2.into(),
            "resume_value" => {
                audit["future_template_correction"]["resume"]["removed"][1] = "luthor/other".into()
            }
            "config" => audit["current_config"]["resume"]["args"][0] = "other".into(),
            "plan" => audit["effective_plan"]["config_revision"] = "corrected-revision".into(),
            "unknown" => audit["future_template_correction"]["resume"]["unknown"] = true.into(),
            _ => unreachable!(),
        }
        reseal(&lane, &audit);
        assert!(
            lane.f
                .store
                .never_dispatched_context("task-a", "attempt-task-a")
                .is_err(),
            "{mutation}"
        );
        assert!(
            lane.f
                .store
                .amended_dispatch_proof(&context, &lane.f.config, "corrected-revision")
                .is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn future_template_dual_rejects_missing_duplicate_and_mismatched_seals() {
    for sql in [
        "DELETE FROM intents WHERE kind='initial_branch_removal_seal'",
        "DELETE FROM evidence WHERE kind='initial_branch_removed'",
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) SELECT 'duplicate',task_id,attempt_id,kind,detail FROM intents WHERE kind='initial_branch_removal_seal'",
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE kind='initial_branch_removed'",
        "UPDATE intents SET detail=json_set(detail,'$.audit_sequence',999) WHERE kind='initial_branch_removal_seal'",
        "UPDATE evidence SET payload=json_set(payload,'$.current_config_revision','other') WHERE kind='initial_branch_removed'",
    ] {
        let mut lane = dual_fixture();
        amend(&mut lane).unwrap();
        let context = lane
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap();
        database(&lane).execute_batch(sql).unwrap();
        assert!(
            lane.f
                .store
                .never_dispatched_context("task-a", "attempt-task-a")
                .is_err(),
            "{sql}"
        );
        assert!(
            lane.f
                .store
                .amended_dispatch_proof(&context, &lane.f.config, "corrected-revision")
                .is_err(),
            "{sql}"
        );
    }
}
