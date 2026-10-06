use super::{FakeGithub, FakeLauncher, Fixture};
use luthor::state::{scheduling, task_records};
use luthor::{
    WorktreeOwner, coordinator::DispatchError, state::StateError, supervisor::SupervisorError,
};
use std::os::unix::fs::PermissionsExt;

pub(crate) fn paused_resume_uses_stored_selection_and_launches_one_continuation() {
    let mut f = Fixture::new(1);
    f.pause();
    let mut github = FakeGithub::new(&f.candidate);
    github.assigned = true;
    f.config.assignment_login = "other".into();
    f.config.resume.args.clear();
    let mut launcher = FakeLauncher::default();
    let plan = f.resume(&mut github, &mut launcher).unwrap();
    assert_eq!(github.reads, 1);
    assert_eq!(github.prs.lookups, 1);
    assert_eq!(launcher.plans.as_slice(), std::slice::from_ref(&plan));
    assert_eq!(plan.attempt_id, "attempt-next");
    assert_eq!(plan.session_id, "task-a");
    assert_eq!(plan.config_revision, "revision");
    let selection = task_records::selection_evidence(&f.store, "task-a")
        .unwrap()
        .unwrap();
    assert_eq!(selection.candidate.source, f.candidate.source);
    assert_eq!(selection.effective_config.assignment_login, "bot");
    assert!(plan.args.iter().any(|arg| arg.contains("continue")));
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-next")
    );
    assert_eq!(
        task_records::task_phase(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(launcher.plans.len(), 1);
}

pub(crate) fn resume_pr_present_or_failed_lookup_holds_without_new_attempt() {
    for fail in [false, true] {
        let mut f = Fixture::new(1);
        f.pause();
        let mut github = FakeGithub::new(&f.candidate);
        github.assigned = true;
        if fail {
            github.prs.fail_on = Some(1);
        } else {
            github.prs.present_on = Some(1);
        }
        let mut launcher = FakeLauncher::default();
        let result = f.resume(&mut github, &mut launcher);
        assert!(if fail {
            matches!(result, Err(DispatchError::PullRequest(_)))
        } else {
            matches!(result, Err(DispatchError::ExistingPr))
        });
        assert_eq!(
            task_records::held_reason(&f.store, "task-a")
                .unwrap()
                .as_deref(),
            Some(if fail {
                "resume PR read failed"
            } else {
                "resume PR present"
            })
        );
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            task_records::latest_attempt(&f.store, "task-a")
                .unwrap()
                .as_deref(),
            Some("attempt-task-a")
        );
        assert!(launcher.plans.is_empty());
    }
}

pub(crate) fn resume_changed_claim_holds_without_new_attempt() {
    let mut f = Fixture::new(1);
    f.pause();
    let mut github = FakeGithub::new(&f.candidate);
    github.change_on = Some(1);
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::ChangedClaim)
    ));
    assert_eq!(github.prs.lookups, 0);
    assert_eq!(
        task_records::held_reason(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("resume claim changed")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-task-a")
    );
    assert!(launcher.plans.is_empty());
}

pub(crate) fn resume_without_paused_reconciled_state_cannot_create_attempt() {
    let mut f = Fixture::new(1);
    let mut github = FakeGithub::new(&f.candidate);
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(task_records::task_count(&f.store).unwrap(), 0);
    f.pause();
    task_records::set_task_phase(&mut f.store, "task-a", "held").unwrap();
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(github.reads, 0);
    assert_eq!(github.prs.lookups, 0);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-task-a")
    );
    assert!(launcher.plans.is_empty());
}

pub(crate) fn failed_resume_dispatch_retains_reservation_and_never_retries() {
    let mut f = Fixture::new(1);
    f.pause();
    let mut github = FakeGithub::new(&f.candidate);
    github.assigned = true;
    let mut launcher = FakeLauncher {
        fail: true,
        ..Default::default()
    };
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::Supervisor(_))
    ));
    assert_eq!(launcher.plans.len(), 1);
    assert_eq!(
        task_records::held_reason(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("resume preparation or dispatch failed")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-next")
    );
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(launcher.plans.len(), 1);
}

pub(crate) fn resume_refuses_missing_owner_protocol_rows_without_state_changes() {
    for mutation in [
        "missing_proof",
        "duplicate_proof",
        "malformed_proof",
        "missing_dispatch",
        "duplicate_dispatch",
        "malformed_dispatch",
        "mismatched_dispatch",
    ] {
        let mut f = Fixture::new(1);
        f.pause();
        let connection =
            rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
        let baseline: (i64, i64) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM evidence WHERE task_id='task-a' AND kind='worktree_owner_protocol'), (SELECT COUNT(*) FROM intents WHERE task_id='task-a' AND kind='supervisor_dispatch')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(baseline, (1, 1), "{mutation} baseline");
        match mutation {
            "missing_proof" => {
                connection.execute("DELETE FROM evidence WHERE task_id='task-a' AND kind='worktree_owner_protocol'", []).unwrap();
            }
            "duplicate_proof" => {
                connection.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE task_id='task-a' AND kind='worktree_owner_protocol'", []).unwrap();
            }
            "malformed_proof" => {
                connection.execute("UPDATE evidence SET payload='{' WHERE task_id='task-a' AND kind='worktree_owner_protocol'", []).unwrap();
            }
            "missing_dispatch" => {
                connection
                    .execute(
                        "DELETE FROM intents WHERE task_id='task-a' AND kind='supervisor_dispatch'",
                        [],
                    )
                    .unwrap();
            }
            "duplicate_dispatch" => {
                connection.execute("INSERT INTO intents(id,task_id,attempt_id,kind,detail) SELECT 'duplicate',task_id,attempt_id,kind,detail FROM intents WHERE task_id='task-a' AND kind='supervisor_dispatch'", []).unwrap();
            }
            "malformed_dispatch" => {
                connection.execute("UPDATE intents SET detail='{' WHERE task_id='task-a' AND kind='supervisor_dispatch'", []).unwrap();
            }
            "mismatched_dispatch" => {
                connection.execute("UPDATE intents SET detail='{}' WHERE task_id='task-a' AND kind='supervisor_dispatch'", []).unwrap();
            }
            _ => unreachable!(),
        }
        let expected: (i64, i64) = match mutation {
            "missing_proof" => (0, 1),
            "duplicate_proof" => (2, 1),
            "missing_dispatch" => (1, 0),
            "duplicate_dispatch" => (1, 2),
            _ => baseline,
        };
        let mutated: (i64, i64) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM evidence WHERE task_id='task-a' AND kind='worktree_owner_protocol'), (SELECT COUNT(*) FROM intents WHERE task_id='task-a' AND kind='supervisor_dispatch')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(mutated, expected, "{mutation} mutation");
        drop(connection);
        assert_resume_rejects_without_changes(&mut f, mutation);
    }
}

fn assert_resume_rejects_without_changes(f: &mut Fixture, mutation: &str) {
    let before = persisted_resume_rows(f);
    let mut github = FakeGithub::new(&f.candidate);
    let mut launcher = FakeLauncher::default();
    assert!(
        matches!(
            f.resume(&mut github, &mut launcher),
            Err(DispatchError::Supervisor(_))
        ),
        "{mutation}"
    );
    assert_eq!(github.reads, 0, "{mutation}");
    assert_eq!(github.prs.lookups, 0, "{mutation}");
    assert!(launcher.plans.is_empty(), "{mutation}");
    assert_eq!(persisted_resume_rows(f), before, "{mutation}");
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-task-a")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}

fn persisted_resume_rows(f: &Fixture) -> Vec<String> {
    let connection = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
    let mut rows = Vec::new();
    for (table, query) in [
        ("tasks", "SELECT * FROM tasks ORDER BY id"),
        ("attempts", "SELECT * FROM attempts ORDER BY rowid"),
        ("reservations", "SELECT * FROM reservations ORDER BY rowid"),
        ("intents", "SELECT * FROM intents ORDER BY rowid"),
        ("evidence", "SELECT * FROM evidence ORDER BY rowid"),
    ] {
        let mut statement = connection.prepare(query).unwrap();
        let columns = statement.column_count();
        let records = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|index| row.get::<_, rusqlite::types::Value>(index))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap();
        rows.push(table.to_owned());
        rows.extend(records.map(|row| format!("{table}:{:?}", row.unwrap())));
    }
    rows
}

pub(crate) fn resume_refuses_replacement_inode_without_creating_an_attempt() {
    let mut f = Fixture::new(1);
    f.pause();
    let owner_path = f.store.root().join("worktree-task-a.lock");
    std::fs::remove_file(&owner_path).unwrap();
    std::fs::write(&owner_path, "replacement").unwrap();
    std::fs::set_permissions(&owner_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut github = FakeGithub::new(&f.candidate);
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::Supervisor(_))
    ));
    assert_eq!(github.reads, 0);
    assert_eq!(github.prs.lookups, 0);
    assert!(launcher.plans.is_empty());
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-task-a")
    );
    assert_eq!(std::fs::read(&owner_path).unwrap(), b"replacement");
}

pub(crate) fn busy_owner_refuses_resume_without_mutation_then_same_task_resumes() {
    let mut f = Fixture::new(1);
    f.pause();
    let before = persisted_resume_rows(&f);
    let prior_attempt_rows = before
        .iter()
        .filter(|row| row.starts_with("attempts:"))
        .count();
    let prior_reservation_rows = before
        .iter()
        .filter(|row| row.starts_with("reservations:"))
        .count();
    let owner = WorktreeOwner::acquire(f.store.root(), "task-a").unwrap();
    let mut github = FakeGithub::new(&f.candidate);
    github.assigned = true;
    let mut launcher = FakeLauncher::default();

    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::Supervisor(SupervisorError::Conflict))
    ));
    assert_eq!(github.reads, 0);
    assert_eq!(github.prs.lookups, 0);
    assert!(launcher.plans.is_empty());
    assert_eq!(persisted_resume_rows(&f), before);

    drop(owner);
    let plan = f.resume(&mut github, &mut launcher).unwrap();
    assert_eq!(github.reads, 1);
    assert_eq!(github.prs.lookups, 1);
    assert_eq!(launcher.plans.as_slice(), std::slice::from_ref(&plan));
    assert_eq!(plan.attempt_id, "attempt-next");
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-next")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    let after = persisted_resume_rows(&f);
    assert_eq!(
        after
            .iter()
            .filter(|row| row.starts_with("attempts:"))
            .count(),
        prior_attempt_rows + 1
    );
    assert_eq!(
        after
            .iter()
            .filter(|row| row.starts_with("reservations:"))
            .count(),
        prior_reservation_rows + 1
    );
    assert_eq!(launcher.plans.len(), 1);
}

pub(crate) fn resume_requires_existing_owner_and_refuses_busy_before_remote_reads() {
    for busy in [false, true] {
        let mut f = Fixture::new(1);
        f.pause();
        let owner_path = f.store.root().join("worktree-task-a.lock");
        let owner = if busy {
            Some(WorktreeOwner::acquire(f.store.root(), "task-a").unwrap())
        } else {
            std::fs::remove_file(&owner_path).unwrap();
            None
        };
        let mut github = FakeGithub::new(&f.candidate);
        github.assigned = true;
        let mut launcher = FakeLauncher::default();
        assert!(matches!(
            f.resume(&mut github, &mut launcher),
            Err(DispatchError::Supervisor(_))
        ));
        assert_eq!(github.reads, 0);
        assert_eq!(github.prs.lookups, 0);
        assert!(launcher.plans.is_empty());
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            task_records::latest_attempt(&f.store, "task-a")
                .unwrap()
                .as_deref(),
            Some("attempt-task-a")
        );
        if !busy {
            assert!(!owner_path.exists(), "resume must not recreate the lock");
        }
        drop(owner);
    }
}
