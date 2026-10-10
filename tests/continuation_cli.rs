#![cfg(unix)]
use luthor::state::{journal, launches, scheduling, task_records};
mod continuation_cli_support;
use continuation_cli_support::{Fixture, git, wait_for};
use luthor::{state::StateStore, supervisor::ExitReceipt};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, process::Stdio};

fn launch_with_lock_probe(f: &Fixture) -> std::process::Output {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let protected = f.rows(&["tasks", "attempts", "reservations", "intents"]);
    fs::write(f.dir.path().join("block"), "").unwrap();
    for _ in 0..20 {
        let mut child = f
            .command(&f.args())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if f.dir.path().join("observing").exists() {
                assert!(
                    StateStore::open(&f.config.state_root, 1).is_err(),
                    "CLI must hold the exclusive lock during source revalidation"
                );
                assert_eq!(f.count("evidence", "never_dispatched_authorized"), 0);
                fs::write(f.dir.path().join("release"), "").unwrap();
                return child.wait_with_output().unwrap();
            }
            if child.try_wait().unwrap().is_some() {
                let out = child.wait_with_output().unwrap();
                assert!(!out.status.success());
                let value: Value = serde_json::from_slice(&out.stdout).unwrap();
                assert_eq!(value["reason"], "process_unavailable");
                assert_eq!(
                    protected,
                    f.rows(&["tasks", "attempts", "reservations", "intents"])
                );
                assert_eq!(f.count("evidence", "never_dispatched_authorized"), 0);
                assert!(!f.marker.exists());
                assert_eq!(
                    fs::read_dir(f.config.state_root.join("attempts"))
                        .unwrap()
                        .count(),
                    0
                );
                assert!(
                    fs::read(&f.calls).unwrap().is_empty(),
                    "first local inspection must refuse before GH reads"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "CLI did not reach source revalidation"
            );
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "production process inspector remained unavailable before lock probe: {}",
        f.process_diagnostic()
    );
}

#[test]
fn continuation_cli_launches_exact_saved_attempt_once_under_exclusive_lock() {
    let f = Fixture::new();
    f.correct_storage();
    let original = f.rows(&["tasks", "attempts", "reservations", "intents", "evidence"]);
    let (out, calls) = f.retry_os_uncertainty(&f.args(), launch_with_lock_probe(&f));
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        result,
        json!({"task_id":f.task,"attempt_id":f.attempt,"config_revision":"saved-revision","status":"dispatched"})
    );
    let receipt_path = f
        .config
        .state_root
        .join(format!("attempts/{}.receipt.json", f.attempt));
    wait_for(&receipt_path);
    let receipt: ExitReceipt = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(receipt.attempt_id, f.attempt);
    assert_eq!(receipt.exit_code, Some(0));
    assert_eq!(receipt.signal, None);
    let marker = fs::read_to_string(&f.marker).unwrap();
    let expected = std::iter::once(f.plan.worktree.to_str().unwrap())
        .chain(f.plan.args.iter().map(String::as_str))
        .map(|value| format!("{value}\n"))
        .collect::<String>();
    assert_eq!(
        marker, expected,
        "worker must execute the exact saved argv in its saved worktree"
    );
    assert_saved_launch(&f);
    let after = f.rows(&["tasks", "attempts", "reservations", "intents", "evidence"]);
    assert_eq!(&after[..3], &original[..3]);
    for table in 3..5 {
        assert!(
            original[table].iter().all(|row| after[table].contains(row)),
            "original rows must remain unchanged"
        );
    }
    git(&f.plan.worktree, &["diff", "--exit-code"]);
    assert_replay_refused(&f, &marker);
    assert_read_calls(&calls);
}

fn assert_saved_launch(f: &Fixture) {
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        task_records::latest_attempt(&store, &f.task)
            .unwrap()
            .as_deref(),
        Some(f.attempt.as_str())
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(
        launches::launch_intent(&store, &f.attempt)
            .unwrap()
            .as_deref(),
        Some(f.saved.as_str())
    );
    let audit: Value = serde_json::from_str(
        &journal::evidence_payloads(&store, &f.task, &f.attempt, "never_dispatched_authorized")
            .unwrap()[0],
    )
    .unwrap();
    assert_eq!(audit["actor"], "acoliver");
    assert_eq!(audit["reason_code"], "legacy_preflight_recovery");
    assert_eq!(audit["saved_launch_plan"], f.saved);
    drop(store);
    let dispatched: String = f
        .db()
        .query_row(
            "SELECT detail FROM intents WHERE kind='supervisor_dispatch'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dispatched, f.saved);
    let persisted: luthor::supervisor::LaunchPlan = serde_json::from_slice(
        &fs::read(
            f.config
                .state_root
                .join(format!("attempts/{}.plan.json", f.attempt)),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(persisted, f.plan);
}

fn assert_replay_refused(f: &Fixture, marker: &str) {
    let before_replay = f.rows(&["tasks", "attempts", "reservations", "intents", "evidence"]);
    let calls = fs::read(&f.calls).unwrap();
    let replay = f.run(&f.args());
    assert!(!replay.status.success());
    let replay: Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay["reason"], "ineligible");
    assert_eq!(
        f.rows(&["tasks", "attempts", "reservations", "intents", "evidence"]),
        before_replay
    );
    assert_eq!(fs::read_to_string(&f.marker).unwrap(), marker);
    assert_eq!(
        fs::read(&f.calls).unwrap(),
        calls,
        "replay must refuse before remote reads"
    );
    assert_eq!(f.count("evidence", "never_dispatched_authorized"), 1);
    assert_eq!(f.count("intents", "supervisor_dispatch"), 1);
    assert!(!String::from_utf8(calls).unwrap().contains(" -X "));
    assert_eq!(
        f.db()
            .query_row("SELECT COUNT(*) FROM attempts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

fn assert_read_calls(calls: &str) {
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("api graphql"))
            .count(),
        2
    );
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("api user"))
            .count(),
        1
    );
    assert_eq!(
        calls.lines().filter(|line| line.contains("pulls?")).count(),
        1
    );
    assert!(!calls.contains(" -X "));
}

#[test]
fn continuation_cli_bad_arguments_refuse_before_config_read_or_writable_open() {
    let f = Fixture::new();
    let before = f.rows(&[
        "tasks",
        "attempts",
        "reservations",
        "intents",
        "evidence",
        "state_meta",
    ]);
    fs::set_permissions(&f.config.state_root, fs::Permissions::from_mode(0o755)).unwrap();
    let good = f.args();
    let mut bad = vec![vec![], good[..12].to_vec()];
    for index in [0, 2, 4, 6, 8, 10] {
        let mut missing = good.clone();
        missing.drain(index..index + 2);
        bad.push(missing);
        let mut duplicate = good.clone();
        duplicate.extend_from_slice(&good[index..index + 2]);
        bad.push(duplicate);
        for invalid in ["", "   ", "--execute", "--unknown"] {
            let mut empty = good.clone();
            empty[index + 1] = invalid.into();
            bad.push(empty);
        }
        let mut unknown = good.clone();
        unknown[index] = "--unknown".into();
        bad.push(unknown);
    }
    let mut duplicate_execute = good.clone();
    duplicate_execute.push("--execute".into());
    bad.push(duplicate_execute);
    let mut reason = good.clone();
    reason[11] = "something_else".into();
    bad.push(reason);
    let mut positional = good.clone();
    positional[0] = f.task.clone();
    bad.push(positional);
    let mut trailing = good.clone();
    trailing.push("unexpected".into());
    bad.push(trailing);
    fs::rename(&f.config_path, f.dir.path().join("hidden-config.json")).unwrap();
    for args in bad {
        let out = f.run(&args);
        assert!(!out.status.success(), "accepted {args:?}");
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("expected --task"),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(
        f.rows(&[
            "tasks",
            "attempts",
            "reservations",
            "intents",
            "evidence",
            "state_meta"
        ]),
        before
    );
    assert_eq!(
        fs::metadata(&f.config.state_root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "StateStore::open must not repair root permissions for bad arguments"
    );
    assert!(fs::read(&f.calls).unwrap().is_empty());
    assert!(!f.marker.exists());
}

#[test]
fn continuation_cli_unsafe_directory_retains_attempt_slot_without_repair() {
    let f = Fixture::new();
    f.assert_held("storage_unavailable");
    assert_eq!(
        fs::metadata(f.config.state_root.join("attempts"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert!(fs::read(&f.calls).unwrap().is_empty());
}

#[test]
fn continuation_cli_uncertain_pr_retains_attempt_slot() {
    let f = Fixture::new();
    f.correct_storage();
    fs::write(f.dir.path().join("prs.json"), "{\"message\":\"uncertain\"}").unwrap();
    f.assert_held("pr_unavailable");
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert!(calls.contains("api user"));
    assert!(calls.contains("pulls?"));
    assert!(!calls.contains(" -X "));
}

#[test]
fn continuation_cli_changed_claim_retains_attempt_slot() {
    let f = Fixture::new();
    f.correct_storage();
    let path = f.dir.path().join("issue.json");
    let mut issue: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    issue["assignees"] = json!([{"login":"other"}]);
    fs::write(path, issue.to_string()).unwrap();
    f.assert_held("claim_changed");
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert!(!calls.contains("api user"));
    assert!(!calls.contains("pulls?"));
    assert!(!calls.contains(" -X "));
}

#[test]
fn continuation_cli_refuses_missing_or_busy_worktree_owner_before_remote_reads() {
    for hold_owner_fd in [false, true] {
        let f = Fixture::new();
        f.correct_storage();
        let owner_path = f
            .config
            .state_root
            .join(format!("worktree-{}.lock", f.task));
        let owner = if hold_owner_fd {
            Some(luthor::WorktreeOwner::acquire_existing(&f.config.state_root, &f.task).unwrap())
        } else {
            fs::remove_file(&owner_path).unwrap();
            None
        };
        let before = f.rows(&["tasks", "attempts", "reservations", "intents"]);
        let out = f.run(&f.args());
        assert!(!out.status.success());
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["status"], "held");
        assert_eq!(value["reason"], "launch_failed");
        assert_eq!(
            f.rows(&["tasks", "attempts", "reservations", "intents"]),
            before
        );
        assert_eq!(f.count("evidence", "never_dispatched_authorized"), 0);
        assert_eq!(f.count("intents", "supervisor_dispatch"), 0);
        assert!(fs::read(&f.calls).unwrap().is_empty());
        assert!(!f.marker.exists());
        if hold_owner_fd {
            assert!(owner_path.exists());
        } else {
            assert!(
                !owner_path.exists(),
                "refusal must not recreate missing owner file"
            );
        }
        drop(owner);
    }
}
