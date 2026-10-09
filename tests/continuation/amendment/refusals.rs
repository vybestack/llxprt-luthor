use super::{amend, database, fixture, pr};
use luthor::state::{BranchRemovalRequest, PausePrStatus, ProcessQuiescence};
use luthor::state::{journal, launches, scheduling};

#[test]
fn amendment_binds_config_provenance_and_external_inspections() {
    for mutation in [
        "mapping",
        "resume",
        "capacity",
        "initial_extra",
        "initial_executable",
        "root",
        "revision",
        "actor",
        "pr_open",
        "pr_ambiguous",
        "pr_repository",
        "pr_timestamp",
        "process_conflict",
        "process_unavailable",
        "process_timestamp",
    ] {
        let mut lane = fixture();
        let context = lane
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap();
        let mut plan = lane.plan.clone();
        plan.args.drain(2..4);
        let mut current_pr = pr();
        let mut processes = ProcessQuiescence::Clear {
            observed_at_unix_secs: 1,
        };
        let mut revision = "corrected-revision";
        let mut actor = "bot";
        mutate_request(
            mutation,
            &mut lane,
            &mut current_pr,
            &mut processes,
            &mut revision,
            &mut actor,
        );
        assert!(
            lane.f
                .store
                .authorize_initial_branch_removal(
                    &context,
                    &plan,
                    BranchRemovalRequest {
                        actor,
                        config: &lane.f.config,
                        config_revision: revision,
                        pr: &current_pr,
                        processes,
                    }
                )
                .is_err(),
            "{mutation}"
        );
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
}

#[test]
fn amendment_dispatch_binds_current_config_provenance() {
    for mutation in ["mapping", "initial", "resume", "revision"] {
        let mut lane = fixture();
        amend(&mut lane).unwrap();
        let context = lane
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap();
        match mutation {
            "mapping" => lane.f.config.mappings[0].push_remote = "other".into(),
            "initial" => lane.f.config.initial.args.push("extra".into()),
            "resume" => lane.f.config.resume.args.push("extra".into()),
            "revision" => {}
            _ => unreachable!(),
        }
        let revision = if mutation == "revision" {
            "other"
        } else {
            "corrected-revision"
        };
        assert!(
            lane.f
                .store
                .amended_dispatch_proof(&context, &lane.f.config, revision)
                .is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn amendment_requires_exact_unique_native_branch_pair() {
    for mutation in [
        "missing",
        "duplicate",
        "inline",
        "foreign",
        "split",
        "not_native",
    ] {
        let mut lane = fixture();
        match mutation {
            "missing" => {
                lane.plan.args.drain(2..4);
            }
            "duplicate" => lane
                .plan
                .args
                .extend(["--branch".into(), "luthor/task-a".into()]),
            "inline" => {
                lane.plan
                    .args
                    .splice(2..4, ["--branch=luthor/task-a".into()]);
            }
            "foreign" => lane.plan.args[3] = "luthor/foreign".into(),
            "split" => {
                lane.plan.args.insert(3, "--other".into());
            }
            "not_native" => lane.plan.executable = "/bin/sh".into(),
            _ => unreachable!(),
        }
        database(&lane)
            .execute(
                "UPDATE intents SET detail=?1 WHERE kind='launch'",
                [serde_json::to_string(&lane.plan).unwrap()],
            )
            .unwrap();
        assert!(amend(&mut lane).is_err(), "{mutation}");
    }
}

#[test]
fn amendment_missing_altered_or_duplicate_seal_fails_closed() {
    for sql in [
        "DELETE FROM evidence WHERE kind='initial_branch_removed';",
        "DELETE FROM intents WHERE kind='initial_branch_removal_seal';",
        "UPDATE intents SET detail=json_set(detail,'$.audit_sequence',999) WHERE kind='initial_branch_removal_seal';",
        "UPDATE intents SET detail=json_set(detail,'$.audit_payload','{}') WHERE kind='initial_branch_removal_seal';",
        "UPDATE intents SET attempt_id=NULL WHERE kind='initial_branch_removal_seal';",
        "UPDATE intents SET id='foreign-seal' WHERE kind='initial_branch_removal_seal';",
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) SELECT 'duplicate',task_id,attempt_id,kind,detail FROM intents WHERE kind='initial_branch_removal_seal';",
        "UPDATE evidence SET payload=json_set(payload,'$.authorized_at_unix_secs',1) WHERE kind='initial_branch_removed';",
        "UPDATE evidence SET payload=json_set(payload,'$.current_config_revision','other') WHERE kind='initial_branch_removed';",
        "UPDATE evidence SET payload='{}' WHERE kind='initial_branch_removed';",
        "UPDATE evidence SET payload=json_set(payload,'$.unknown',1) WHERE kind='initial_branch_removed';",
        "UPDATE intents SET detail=detail || ' ' WHERE kind='launch';",
        "UPDATE reservations SET created_at='altered';",
    ] {
        let mut lane = fixture();
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
        let owner = luthor::WorktreeOwner::acquire(lane.f.store.root(), "task-a").unwrap();
        let proof = owner
            .protocol_evidence(lane.f.store.root(), "task-a", "attempt-task-a")
            .unwrap();
        assert!(
            launches::begin_supervision(
                &mut lane.f.store,
                "task-a",
                "attempt-task-a",
                context.saved_launch_plan(),
                &proof
            )
            .is_err(),
            "{sql}"
        );
    }
}

#[test]
fn amendment_stale_snapshot_and_audit_or_seal_write_failure_are_atomic() {
    for sql in [
        "INSERT INTO evidence(task_id,kind,payload) VALUES('task-a','held_reason','changed');",
        "CREATE TRIGGER audit_failure BEFORE INSERT ON evidence WHEN NEW.kind='initial_branch_removed' BEGIN SELECT RAISE(ABORT,'injected'); END;",
        "CREATE TRIGGER seal_failure BEFORE INSERT ON intents WHEN NEW.kind='initial_branch_removal_seal' BEGIN SELECT RAISE(ABORT,'injected'); END;",
    ] {
        let mut lane = fixture();
        let context = lane
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap();
        let mut effective = lane.plan.clone();
        effective.args.drain(2..4);
        let db = database(&lane);
        db.execute_batch(sql).unwrap();
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
                .is_err()
        );
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM evidence WHERE kind='initial_branch_removed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM intents WHERE kind='initial_branch_removal_seal'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
        assert_eq!(
            launches::launch_intent(&lane.f.store, "attempt-task-a")
                .unwrap()
                .unwrap(),
            context.saved_launch_plan()
        );
    }
}

#[test]
fn amendment_cannot_weaken_normal_dispatch_plan_equality() {
    let mut lane = super::super::Lane::new();
    let saved = launches::launch_intent(&lane.f.store, "attempt-task-a")
        .unwrap()
        .unwrap();
    let mut altered = lane.plan.clone();
    altered.args.push("extra".into());
    let owner = luthor::WorktreeOwner::acquire(lane.f.store.root(), "task-a").unwrap();
    let proof = owner
        .protocol_evidence(lane.f.store.root(), "task-a", "attempt-task-a")
        .unwrap();
    assert!(
        launches::begin_supervision(
            &mut lane.f.store,
            "task-a",
            "attempt-task-a",
            &serde_json::to_string(&altered).unwrap(),
            &proof
        )
        .is_err()
    );
    launches::begin_supervision(
        &mut lane.f.store,
        "task-a",
        "attempt-task-a",
        &saved,
        &proof,
    )
    .unwrap();
    assert!(
        launches::begin_supervision(
            &mut lane.f.store,
            "task-a",
            "attempt-task-a",
            &saved,
            &proof
        )
        .is_err()
    );
    assert_eq!(
        launches::launch_intent(&lane.f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        saved
    );
}

fn mutate_request(
    mutation: &str,
    lane: &mut super::Lane,
    current_pr: &mut luthor::state::ExitPrEvidence,
    processes: &mut ProcessQuiescence,
    revision: &mut &str,
    actor: &mut &str,
) {
    match mutation {
        "mapping" => lane.f.config.mappings[0].push_remote = "other".into(),
        "resume" => lane.f.config.resume.args.push("extra".into()),
        "capacity" => lane.f.config.capacity = 2,
        "initial_extra" => lane.f.config.initial.args.push("extra".into()),
        "initial_executable" => lane.f.config.initial.executable = "/other".into(),
        "root" => lane.f.config.state_root = "/other".into(),
        "revision" => *revision = "",
        "actor" => *actor = "other",
        "pr_open" => current_pr.status = PausePrStatus::Open,
        "pr_ambiguous" => current_pr.status = PausePrStatus::Ambiguous,
        "pr_repository" => current_pr.repository = "org/other".into(),
        "pr_timestamp" => current_pr.observed_at_unix_secs = 0,
        "process_conflict" => {
            *processes = ProcessQuiescence::Conflict {
                observed_at_unix_secs: 1,
            }
        }
        "process_unavailable" => *processes = ProcessQuiescence::Unavailable,
        "process_timestamp" => {
            *processes = ProcessQuiescence::Clear {
                observed_at_unix_secs: 0,
            }
        }
        _ => unreachable!(),
    }
}
