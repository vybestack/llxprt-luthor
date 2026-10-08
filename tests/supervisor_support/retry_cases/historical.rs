use super::*;
use luthor::WorktreeOwner;
use luthor::state::{journal, launches, scheduling, task_records};

#[cfg(unix)]
pub(crate) fn historical_startup_exit_revalidation_launches_once_preserving_all_old_rows_and_files()
{
    for old_boot in [
        "{ sec = 1790533213, usec = 116017 } Sun Sep 27 15:20:13 2026",
        "{ sec = 1790533213, usec = 220969 } Sun Sep 27 15:20:13 2026",
    ] {
        let (_dir, config, mut store) = historical_retry_fixture_with_boot(old_boot);
        let original = task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap();
        let rows = old_retry_rows(&config);
        let files = snapshot_attempt_files(&config);
        let mut projects = OtherProject(original.candidate, 1);
        let mut prs = ExitPr::default();
        let mut launcher = OtherLauncher::default();
        assert!(retry(&mut store, &config, &mut projects, &mut prs, &mut launcher).is_err());
        let plan =
            historical_retry(&mut store, &config, &mut projects, &mut prs, &mut launcher).unwrap();
        assert_eq!(launcher.0, 1);
        assert_eq!(prs.reads, 1);
        assert_eq!(plan.session_id, "task");
        assert_eq!(old_retry_rows(&config), rows);
        assert_attempt_files_unchanged(files);
        let audit: serde_json::Value = serde_json::from_str(
            &journal::evidence_payloads(&store, "task", "attempt-retry", "retry_authorized")
                .unwrap()[0],
        )
        .unwrap();
        assert_eq!(audit["terminal_exit"]["receipt"]["exit_code"], 2);
        assert_eq!(
            audit["terminal_exit"]["basis"],
            "native_max_tool_calls_preflight"
        );
        assert_eq!(audit["terminal_exit"]["receipt"]["boot_identity"], old_boot);
        assert!(
            audit["terminal_exit"]["observed_at_unix_secs"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(
            audit["source"]["issue"]["assignees"],
            serde_json::json!(["operator"])
        );
        assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
        assert!(
            historical_retry(&mut store, &config, &mut projects, &mut prs, &mut launcher).is_err()
        );
        let owner = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
        execute_with_binary(
            &mut store,
            &plan,
            Path::new(env!("CARGO_BIN_EXE_luthor")),
            &owner,
        )
        .unwrap();
        drop(owner);
        let path = config
            .state_root
            .join("attempts/attempt-retry.receipt.json");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        super::super::wait_for_worktree_owner_release(&config.state_root, "task");
        let reconciliation = reconcile_attempt(&mut store, "task", "attempt-retry").unwrap();
        assert_historical_retry_completed(reconciliation, &path, &store);
        assert_eq!(old_retry_rows(&config), rows);
    }
}

#[cfg(unix)]
fn assert_historical_retry_completed(
    reconciliation: Reconciliation,
    receipt_path: &std::path::Path,
    store: &StateStore,
) {
    assert!(
        matches!(
            reconciliation,
            Reconciliation::Completed {
                exit_code: Some(2),
                signal: None
            }
        ),
        "expected Completed {{ exit_code: Some(2), signal: None }}; got {reconciliation:?}; receipt file: {:?}; persisted exit: {:?}",
        fs::read(receipt_path),
        luthor::state::journal::evidence_payloads(store, "task", "attempt-retry", "attempt_exit"),
    );
}

#[cfg(unix)]
fn snapshot_attempt_files(config: &Config) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fs::read_dir(config.state_root.join("attempts"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .map(|path| (path.clone(), fs::read(path).unwrap()))
        .collect()
}

#[cfg(unix)]
fn assert_attempt_files_unchanged(files: Vec<(std::path::PathBuf, Vec<u8>)>) {
    for (path, bytes) in files {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[cfg(unix)]
pub(crate) fn historical_revalidation_refuses_unknown_runtime_and_live_or_reused_registered_processes()
 {
    for refusal in [
        "diagnostic",
        "log_length",
        "session",
        "receipt_missing",
        "gate_sent_missing",
        "exit_code",
        "child_live",
        "child_reused",
        "supervisor_live",
        "supervisor_reused",
        "tracked_escaped",
        "log_symlink",
        "boot_contradiction",
        "child_group",
    ] {
        let (_dir, config, mut store) = historical_retry_fixture();
        apply_historical_refusal(refusal, &config, &mut store);
        let rows = old_retry_rows(&config);
        let selection = task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap();
        let mut projects = OtherProject(selection.candidate, 1);
        let mut prs = ExitPr::default();
        let mut launcher = OtherLauncher::default();
        assert!(
            historical_retry(&mut store, &config, &mut projects, &mut prs, &mut launcher).is_err(),
            "{refusal}"
        );
        assert_eq!(launcher.0, 0, "{refusal}");
        assert_eq!(prs.reads, 0, "{refusal}");
        assert_eq!(projects.1, 1, "{refusal}");
        assert_eq!(
            scheduling::reservation_count(&store).unwrap(),
            0,
            "{refusal}"
        );
        assert_eq!(
            task_records::latest_attempt(&store, "task")
                .unwrap()
                .as_deref(),
            Some("attempt-real")
        );
        assert_eq!(old_retry_rows(&config), rows, "{refusal}");
    }
}

#[cfg(unix)]
pub(crate) fn historical_exit_handoff_refuses_surviving_groups_even_with_reaped_leaders() {
    use std::os::unix::process::CommandExt;
    struct Group(i32);
    impl Drop for Group {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }
    for who in ["child", "supervisor"] {
        let (_dir, config, mut store) = historical_retry_fixture();
        let mut leader = Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 30 >/dev/null 2>&1 &"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = leader.id();
        let _group = Group(pid as i32);
        leader.wait().unwrap();
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
        assert_eq!(
            unsafe { libc::kill(-(pid as i32), 0) },
            0,
            "fixture group absent"
        );
        let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
        let receipt_file = receipt_path(&config);
        let mut receipt: luthor::supervisor::ExitReceipt =
            serde_json::from_slice(&fs::read(&receipt_file).unwrap()).unwrap();
        if who == "child" {
            let child_file = config.state_root.join("attempts/attempt-real.child.json");
            let mut child: serde_json::Value =
                serde_json::from_slice(&fs::read(&child_file).unwrap()).unwrap();
            child["pid"] = pid.into();
            child["group_id"] = pid.into();
            receipt.child_pid = pid;
            fs::write(child_file, serde_json::to_vec(&child).unwrap()).unwrap();
            fs::write(receipt_file, serde_json::to_vec(&receipt).unwrap()).unwrap();
            db.execute("UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-real' AND kind='child_registered'", [child.to_string()]).unwrap();
            db.execute("UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-real' AND kind='attempt_exit'", [serde_json::to_string(&receipt).unwrap()]).unwrap();
        } else {
            let ready: String = db.query_row("SELECT payload FROM evidence WHERE attempt_id='attempt-real' AND kind='supervisor_ready'", [], |row| row.get(0)).unwrap();
            let mut ready: serde_json::Value = serde_json::from_str(&ready).unwrap();
            ready["pid"] = pid.into();
            db.execute("UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-real' AND kind IN ('supervisor_ready','gate_sent')", [ready.to_string()]).unwrap();
            db.execute("UPDATE intents SET detail=?1 WHERE attempt_id='attempt-real' AND kind='gate_release'", [ready.to_string()]).unwrap();
        }
        let rows = old_retry_rows(&config);
        let selection = task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap();
        let mut projects = OtherProject(selection.candidate, 1);
        let mut prs = ExitPr::default();
        let mut launcher = OtherLauncher::default();
        let error = historical_retry(&mut store, &config, &mut projects, &mut prs, &mut launcher)
            .unwrap_err();
        assert!(
            matches!(error, luthor::coordinator::DispatchError::RetryHeld { reason } if reason == "registered process group is present or its absence is unproven")
        );
        assert_eq!(launcher.0, 0);
        assert_eq!(prs.reads, 0);
        assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
        assert_eq!(old_retry_rows(&config), rows);
    }
}

#[cfg(unix)]
pub(crate) fn historical_handoff_requires_exhaustive_pr_absence_and_rechecks_transaction_state() {
    struct Pr {
        pages: usize,
        conflict: bool,
        state: std::path::PathBuf,
    }
    impl PullRequestReader for Pr {
        fn authenticated_identity(&mut self) -> Result<String, LookupError> {
            Ok("operator".into())
        }
        fn page(&mut self, _: &str, page: u32) -> Result<Vec<serde_json::Value>, LookupError> {
            self.pages += 1;
            if page == 1 {
                return Ok((1..=100)
                    .map(|n| serde_json::json!({"number":n,"body":"unrelated issue"}))
                    .collect());
            }
            if !self.conflict {
                return Err(LookupError {
                    category: ErrorCategory::Transport,
                    code: "offline",
                    status: None,
                });
            }
            let db = rusqlite::Connection::open(self.state.join("state.sqlite3")).unwrap();
            db.execute("UPDATE tasks SET state='held' WHERE id='task'", [])
                .unwrap();
            Ok(vec![])
        }
        fn detail(&mut self, _: &str, _: u64) -> Result<serde_json::Value, LookupError> {
            panic!("unlinked PR detail requested")
        }
        fn repository_identity(&mut self, _: &str) -> Result<u64, LookupError> {
            panic!("unlinked PR identity requested")
        }
    }
    for conflict in [false, true] {
        let (_dir, config, mut store) = historical_retry_fixture();
        let rows = old_retry_rows(&config);
        let selection = task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap();
        let mut projects = OtherProject(selection.candidate, 1);
        let mut prs = Pr {
            pages: 0,
            conflict,
            state: config.state_root.clone(),
        };
        let mut launcher = OtherLauncher::default();
        let result = luthor::coordinator::retry_one(
            &mut store,
            luthor::coordinator::RetryDependencies {
                task_id: "task",
                previous_attempt_id: "attempt-real",
                attempt_id: "attempt-retry",
                config: &config,
                config_revision: "corrected",
                actor: "operator",
                reason: "correct native startup argv",
                revalidate_terminal_exit: true,
                projects: &mut projects,
                prs: &mut prs,
                launcher: &mut launcher,
            },
        );
        assert!(result.is_err());
        assert_eq!(prs.pages, 2);
        assert_eq!(launcher.0, 0);
        assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
        assert_eq!(old_retry_rows(&config), rows);
        assert!(
            launches::launch_intent(&store, "attempt-retry")
                .unwrap()
                .is_none()
        );
    }
}

fn apply_historical_refusal(refusal: &str, config: &Config, store: &mut StateStore) {
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let child_path = config.state_root.join("attempts/attempt-real.child.json");
    let mut child: serde_json::Value =
        serde_json::from_slice(&fs::read(&child_path).unwrap()).unwrap();
    let mut receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt_path(config)).unwrap()).unwrap();
    match refusal {
        "diagnostic" | "session" => {
            let mut bytes = fs::read(&receipt.stdout_path).unwrap();
            let needle = if refusal == "session" {
                b"task".as_slice()
            } else {
                b"max-tool-calls".as_slice()
            };
            let pos = bytes
                .windows(needle.len())
                .position(|b| b == needle)
                .unwrap();
            bytes[pos] = b'X';
            fs::write(&receipt.stdout_path, bytes).unwrap();
        }
        "log_length" => {
            fs::write(&receipt.stdout_path, "runtime could have executed tools").unwrap();
        }
        "receipt_missing" => {
            fs::remove_file(receipt_path(config)).unwrap();
        }
        "gate_sent_missing" => {
            db.execute(
                "DELETE FROM evidence WHERE attempt_id='attempt-real' AND kind='gate_sent'",
                [],
            )
            .unwrap();
        }
        "exit_code" => {
            receipt.exit_code = Some(7);
            fs::write(receipt_path(config), serde_json::to_vec(&receipt).unwrap()).unwrap();
            db.execute("UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-real' AND kind='attempt_exit'", [serde_json::to_string(&receipt).unwrap()]).unwrap();
            db.execute("UPDATE attempts SET outcome='exit_code=Some(7);signal=None' WHERE id='attempt-real'", []).unwrap();
        }
        "child_live" | "child_reused" => {
            child["pid"] = std::process::id().into();
            child["group_id"] = child["pid"].clone();
            if refusal == "child_reused" {
                child["start_identity"] = "different historical start".into();
            }
            receipt.child_pid = std::process::id();
            receipt.child_start_identity = child["start_identity"].as_str().unwrap().into();
            fs::write(&child_path, serde_json::to_vec(&child).unwrap()).unwrap();
            fs::write(receipt_path(config), serde_json::to_vec(&receipt).unwrap()).unwrap();
            db.execute("UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-real' AND kind='child_registered'", [child.to_string()]).unwrap();
            db.execute("UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-real' AND kind='attempt_exit'", [serde_json::to_string(&receipt).unwrap()]).unwrap();
        }
        "supervisor_live" | "supervisor_reused" => {
            let payload = serde_json::json!({"pid":std::process::id(), "boot_identity":receipt.boot_identity, "start_identity":if refusal == "supervisor_reused" {"different historical start"} else {"original start"}}).to_string();
            db.execute("UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-real' AND kind IN ('supervisor_ready','gate_sent')", [&payload]).unwrap();
            db.execute("UPDATE intents SET detail=?1 WHERE attempt_id='attempt-real' AND kind='gate_release'", [&payload]).unwrap();
        }
        "tracked_escaped" => {
            journal::record_evidence(store, "task", Some("attempt-real"), "tracked_descendant", &serde_json::json!({"pid":std::process::id(),"boot_identity":receipt.boot_identity,"start_identity":"escaped-start"}).to_string()).unwrap();
        }
        "log_symlink" => {
            let content = fs::read(&receipt.stdout_path).unwrap();
            let other = config.state_root.join("other.log");
            fs::write(&other, content).unwrap();
            fs::remove_file(&receipt.stdout_path).unwrap();
            std::os::unix::fs::symlink(other, &receipt.stdout_path).unwrap();
        }
        "boot_contradiction" => {
            db.execute("UPDATE evidence SET payload=json_set(payload,'$.boot_identity','different') WHERE attempt_id='attempt-real' AND kind='supervisor_ready'", []).unwrap();
        }
        "child_group" => {
            child["group_id"] = std::process::id().into();
            fs::write(&child_path, serde_json::to_vec(&child).unwrap()).unwrap();
            db.execute("UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-real' AND kind='child_registered'", [child.to_string()]).unwrap();
        }
        _ => unreachable!(),
    }
}
