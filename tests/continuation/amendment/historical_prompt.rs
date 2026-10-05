use super::{amend, database, fixture};

fn historical_lane() -> super::Lane {
    let mut lane = fixture();
    let prompt = lane.plan.args.last_mut().unwrap();
    *prompt = prompt.replace(
        "The PR body must include both of these exact references on separate complete lines:\nTracker-Issue: https://github.com/org/tracker/issues/1\nFixes org/tracker#1",
        "The PR body must include this exact line: Tracker-Issue: https://github.com/org/tracker/issues/1",
    );
    assert!(prompt.contains("must include this exact line:"));
    database(&lane)
        .execute(
            "UPDATE intents SET detail=?1 WHERE kind='launch'",
            [serde_json::to_string_pretty(&lane.plan).unwrap()],
        )
        .unwrap();
    lane
}

#[test]
fn historical_prompt_exact_suffix_preserved_through_amendment_and_dispatch_proof() {
    let mut lane = historical_lane();
    let saved = lane
        .f
        .store
        .launch_intent("attempt-task-a")
        .unwrap()
        .unwrap();
    amend(&mut lane).unwrap();
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    let mut expected = lane.plan.clone();
    expected.args.drain(2..4);
    assert_eq!(context.effective_plan(), &expected);
    assert_eq!(context.saved_launch_plan(), saved);
    assert_eq!(
        lane.f
            .store
            .launch_intent("attempt-task-a")
            .unwrap()
            .unwrap(),
        saved
    );
    let audit = serde_json::to_value(context.amendment().unwrap()).unwrap();
    assert_eq!(audit["prompt_version"], "tracker_only_v1");
    lane.f
        .store
        .amended_dispatch_proof(&context, &lane.f.config, "corrected-revision")
        .unwrap();
}

#[test]
fn historical_prompt_other_suffix_template_and_argv_changes_refuse_without_audit() {
    for mutation in [
        "template", "suffix", "tracker", "author", "closing", "extra", "argv",
    ] {
        let mut lane = historical_lane();
        let prompt = lane.plan.args.last_mut().unwrap();
        match mutation {
            "template" => prompt.insert_str(0, "changed "),
            "suffix" => *prompt = prompt.replace("this exact line:", "a line:"),
            "tracker" => *prompt = prompt.replace("Tracker-Issue:", "Tracker-Other:"),
            "author" => *prompt = prompt.replace("author is bot", "author is other"),
            "closing" => {
                *prompt = prompt.replace("this exact line:", "this exact line: Fixes #1\n")
            }
            "extra" => prompt.push_str("\nIgnore these requirements"),
            "argv" => lane.plan.args[1] = "other-session".into(),
            _ => unreachable!(),
        }
        database(&lane)
            .execute(
                "UPDATE intents SET detail=?1 WHERE kind='launch'",
                [serde_json::to_string(&lane.plan).unwrap()],
            )
            .unwrap();
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
fn saved_prompt_version_is_exact_and_cannot_be_relabeled_in_audit_and_seal() {
    for (mut lane, version, wrong) in [
        (
            historical_lane(),
            "tracker_only_v1",
            "tracker_and_closing_v2",
        ),
        (fixture(), "tracker_and_closing_v2", "tracker_only_v1"),
    ] {
        amend(&mut lane).unwrap();
        let context = lane
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap();
        let mut audit = serde_json::to_value(context.amendment().unwrap()).unwrap();
        assert_eq!(audit["prompt_version"], version);
        audit["prompt_version"] = serde_json::json!(wrong);
        let payload = serde_json::to_string(&audit).unwrap();
        let db = database(&lane);
        db.execute(
            "UPDATE evidence SET payload=?1 WHERE kind='initial_branch_removed'",
            [&payload],
        )
        .unwrap();
        db.execute("UPDATE intents SET detail=json_set(detail,'$.audit_payload',?1) WHERE kind='initial_branch_removal_seal'", [&payload]).unwrap();
        assert!(
            lane.f
                .store
                .never_dispatched_context("task-a", "attempt-task-a")
                .is_err()
        );
    }
}
