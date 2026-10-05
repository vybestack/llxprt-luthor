use super::{database, fixture, pr};
use luthor::state::{BranchRemovalRequest, ProcessQuiescence};
use luthor::state::{journal, task_records};

#[test]
fn amendment_refuses_every_single_or_other_adjacent_pair_deletion() {
    let mut lane = fixture();
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    for width in [1, 2] {
        for index in 0..=lane.plan.args.len() - width {
            if width == 2 && index == 2 {
                continue;
            }
            let mut effective = lane.plan.clone();
            effective.args.drain(index..index + width);
            assert!(
                lane.f
                    .store
                    .authorize_initial_branch_removal(
                        &context,
                        &effective,
                        BranchRemovalRequest {
                            actor: "bot",
                            config: &lane.f.config,
                            config_revision: "corrected-revision",
                            pr: &pr(),
                            processes: ProcessQuiescence::Clear {
                                observed_at_unix_secs: 1
                            },
                        }
                    )
                    .is_err(),
                "index={index}, width={width}"
            );
        }
    }
    assert!(
        journal::evidence_payloads(
            &lane.f.store,
            "task-a",
            "attempt-task-a",
            "initial_branch_removed"
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn amendment_bounds_audit_and_rejects_foreign_store() {
    let mut lane = fixture();
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    let mut other = fixture();
    let mut effective = lane.plan.clone();
    effective.args.drain(2..4);
    assert!(
        other
            .f
            .store
            .authorize_initial_branch_removal(
                &context,
                &effective,
                BranchRemovalRequest {
                    actor: "bot",
                    config: &lane.f.config,
                    config_revision: "corrected-revision",
                    pr: &pr(),
                    processes: ProcessQuiescence::Clear {
                        observed_at_unix_secs: 1
                    },
                }
            )
            .is_err()
    );
    let mut selection = task_records::selection_evidence(&lane.f.store, "task-a")
        .unwrap()
        .unwrap();
    let huge = "x".repeat(262_144);
    selection.effective_config.initial.args.push(huge.clone());
    lane.f.config.initial.args.push(huge.clone());
    lane.plan.args.push(huge);
    let db = database(&lane);
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
    assert!(super::amend(&mut lane).is_err());
    assert!(
        journal::evidence_payloads(
            &lane.f.store,
            "task-a",
            "attempt-task-a",
            "initial_branch_removed"
        )
        .unwrap()
        .is_empty()
    );
}
