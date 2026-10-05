use luthor::state::{journal, launches, scheduling, task_records};
mod deletions;
mod dispatch;
mod future_templates;
mod historical_prompt;
#[cfg(unix)]
mod lifecycle;
mod refusals;
use super::Lane;
use luthor::state::{
    AmendedDispatchProof, BranchRemovalRequest, EffectiveConfigSnapshot, ExitPrEvidence,
    NeverDispatchedReason, PausePrStatus, ProcessQuiescence, verify_amended_worker_plan,
};
use rusqlite::{Connection, params};

pub(super) fn fixture() -> Lane {
    let mut lane = Lane::new();
    lane.f.config.initial.executable = "/native/llxprt-code-rs".into();
    lane.f
        .config
        .initial
        .args
        .splice(2..2, ["--branch".into(), "luthor/{task.id}".into()]);
    lane.plan.executable = lane.f.config.initial.executable.clone();
    lane.plan
        .args
        .splice(2..2, ["--branch".into(), "luthor/task-a".into()]);
    let mut selection = task_records::selection_evidence(&lane.f.store, "task-a")
        .unwrap()
        .unwrap();
    selection.effective_config = EffectiveConfigSnapshot::from(&lane.f.config);
    let db = database(&lane);
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE kind='selection'",
        [serde_json::to_string(&selection).unwrap()],
    )
    .unwrap();
    db.execute(
        "UPDATE intents SET detail=?1 WHERE kind='launch'",
        [serde_json::to_string_pretty(&lane.plan).unwrap()],
    )
    .unwrap();
    lane.f.config.initial.args.drain(2..4);
    lane
}
pub(super) fn database(lane: &Lane) -> Connection {
    Connection::open(lane.f.store.root().join("state.sqlite3")).unwrap()
}
fn pr() -> ExitPrEvidence {
    ExitPrEvidence {
        observed_at_unix_secs: 1,
        repository: "org/code".into(),
        status: PausePrStatus::Absent,
    }
}
pub(super) fn amend(lane: &mut Lane) -> Result<i64, luthor::state::StateError> {
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")?;
    let mut effective = lane.plan.clone();
    effective.args.drain(2..4);
    lane.f.store.authorize_initial_branch_removal(
        &context,
        &effective,
        BranchRemovalRequest {
            actor: "bot",
            config: &lane.f.config,
            config_revision: "corrected-revision",
            pr: &pr(),
            processes: ProcessQuiescence::Clear {
                observed_at_unix_secs: 1,
            },
        },
    )
}

#[test]
fn amendment_exact_deletion_is_append_only_durable_and_single_use() {
    let mut lane = fixture();
    let original = launches::launch_intent(&lane.f.store, "attempt-task-a")
        .unwrap()
        .unwrap();
    let db = database(&lane);
    let before = evidence_rows(&db);
    let seq = amend(&mut lane).unwrap();
    assert!(seq > 0);
    assert_eq!(
        launches::launch_intent(&lane.f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        original
    );
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
    let after = evidence_rows(&db);
    assert_eq!(&after[..before.len()], &before);
    assert_eq!(after.len(), before.len() + 1);
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    assert_eq!(context.plan(), &lane.plan);
    assert_eq!(context.saved_launch_plan(), original);
    let mut effective = lane.plan.clone();
    effective.args.drain(2..4);
    assert_eq!(context.effective_plan(), &effective);
    let proof = lane
        .f
        .store
        .amended_dispatch_proof(&context, &lane.f.config, "corrected-revision")
        .unwrap();
    assert_eq!(proof.amendment_sequence, seq);
    assert_eq!(proof.effective_plan, effective);
    assert!(
        context
            .authorize(
                &mut lane.f.store,
                "bot",
                NeverDispatchedReason::LegacyPreflightRecovery
            )
            .is_err()
    );
    assert!(amend(&mut lane).is_err());
    assert!(
        launches::begin_supervision(
            &mut lane.f.store,
            "task-a",
            "attempt-task-a",
            &serde_json::to_string(&effective).unwrap()
        )
        .is_err()
    );
    drop(lane.f.store);
    lane.f.store = luthor::state::StateStore::open(&lane.f.config.state_root, 1).unwrap();
    assert_eq!(
        lane.f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap()
            .effective_plan(),
        &effective
    );
    assert!(amend(&mut lane).is_err());
}

#[test]
fn amendment_refuses_every_other_plan_change_without_audit() {
    for mutation in [
        "none",
        "other_delete",
        "append",
        "substitute",
        "executable",
        "session",
        "attempt",
        "revision",
        "home",
        "worktree",
        "prompt",
    ] {
        let mut lane = fixture();
        let context = lane
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap();
        let mut plan = lane.plan.clone();
        if mutation != "none" {
            plan.args.drain(2..4);
        }
        match mutation {
            "none" => {}
            "other_delete" => {
                plan.args.remove(0);
            }
            "append" => plan.args.push("--extra".into()),
            "substitute" => plan.args[0] = "different".into(),
            "executable" => plan.executable = "/other".into(),
            "session" => plan.session_id = "other".into(),
            "attempt" => plan.attempt_id = "other".into(),
            "revision" => plan.config_revision = "other".into(),
            "home" => plan.session_environment.home = "/other".into(),
            "worktree" => plan.expected_worktree.head = "other".into(),
            "prompt" => {
                let index = plan
                    .args
                    .iter()
                    .position(|v| v == "-p" || v == "--prompt")
                    .unwrap();
                plan.args[index + 1].push_str("changed");
            }
            _ => unreachable!(),
        }
        assert!(
            lane.f
                .store
                .authorize_initial_branch_removal(
                    &context,
                    &plan,
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
fn amendment_scope_tampering_and_contrary_proofs_fail_closed() {
    for sql in [
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE kind='initial_branch_removed';",
        "UPDATE evidence SET attempt_id='foreign' WHERE kind='initial_branch_removed';",
        "UPDATE evidence SET attempt_id=NULL WHERE kind='initial_branch_removed';",
        "UPDATE evidence SET payload=json_set(payload,'$.effective_plan.args[0]','different') WHERE kind='initial_branch_removed';",
        "UPDATE evidence SET payload=json_set(payload,'$.actor','other') WHERE kind='initial_branch_removed';",
        "UPDATE evidence SET payload=json_set(payload,'$.delta.index',0) WHERE kind='initial_branch_removed';",
        "UPDATE evidence SET payload=json_set(payload,'$.current_config.capacity',2) WHERE kind='initial_branch_removed';",
        "UPDATE evidence SET payload=json_set(payload,'$.pr.status.status','ambiguous') WHERE kind='initial_branch_removed';",
        "UPDATE evidence SET payload=json_set(payload,'$.processes.status','conflict') WHERE kind='initial_branch_removed';",
        "UPDATE intents SET detail=json_set(detail,'$.args[0]','different') WHERE kind='launch';",
        "UPDATE evidence SET payload=json_set(payload,'$.effective_config.resume.args[0]','different') WHERE kind='selection';",
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-a','attempt-task-a','child_registered','{}');",
        "INSERT INTO evidence(task_id,kind,payload) VALUES('task-a','verified_open_pr','{}');",
        "UPDATE reservations SET status='released';",
        "UPDATE tasks SET state='pr_complete';",
        "UPDATE attempts SET lifecycle='completed';",
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
    }
}

#[test]
fn amendment_dispatch_proof_binds_effective_plan_and_audit_sequence() {
    let mut lane = fixture();
    amend(&mut lane).unwrap();
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    let proof = lane
        .f
        .store
        .amended_dispatch_proof(&context, &lane.f.config, "corrected-revision")
        .unwrap();
    let db = database(&lane);
    assert!(verify_amended_worker_plan(&db, lane.f.store.root(), &proof.effective_plan).is_err());
    let committed = lane
        .f
        .store
        .begin_amended_supervision(&context, &lane.f.config, "corrected-revision")
        .unwrap();
    assert_eq!(committed, proof);
    assert!(
        lane.f
            .store
            .begin_amended_supervision(&context, &lane.f.config, "corrected-revision")
            .is_err()
    );
    verify_amended_worker_plan(&db, lane.f.store.root(), &proof.effective_plan).unwrap();
    assert!(
        lane.f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .is_err()
    );
    for kind in ["missing", "sequence", "original", "duplicate", "extra"] {
        let mut corrupt = proof.clone();
        match kind {
            "missing" => {
                db.execute(
                    "DELETE FROM evidence WHERE kind='initial_branch_removed'",
                    [],
                )
                .unwrap();
            }
            "sequence" => corrupt.amendment_sequence += 1,
            "original" => corrupt.effective_plan = lane.plan.clone(),
            "duplicate" => {
                db.execute("INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('second','task-a','attempt-task-a','supervisor_dispatch',?1)", [serde_json::to_string(&proof).unwrap()]).unwrap();
            }
            "extra" => {
                db.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-a','attempt-task-a','unknown','{}')", []).unwrap();
            }
            _ => unreachable!(),
        }
        db.execute(
            "UPDATE intents SET detail=?1 WHERE id='supervisor-attempt-task-a'",
            params![serde_json::to_string(&corrupt).unwrap()],
        )
        .unwrap();
        assert!(
            verify_amended_worker_plan(&db, lane.f.store.root(), &proof.effective_plan).is_err(),
            "{kind}"
        );
        db.execute("DELETE FROM intents WHERE id='second'", [])
            .unwrap();
        db.execute("DELETE FROM evidence WHERE kind='unknown'", [])
            .unwrap();
        if kind == "missing" {
            db.execute("INSERT INTO evidence(sequence,task_id,attempt_id,kind,payload) VALUES(?1,'task-a','attempt-task-a','initial_branch_removed',?2)", params![proof.amendment_sequence, serde_json::to_string(context.amendment().unwrap()).unwrap()]).unwrap();
        }
    }
    let _: AmendedDispatchProof = proof;
}

fn evidence_rows(db: &Connection) -> Vec<String> {
    db.prepare("SELECT payload FROM evidence ORDER BY sequence")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
