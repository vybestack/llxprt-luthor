use super::{AmendmentFixture, StateStore, Value, json, wait_for};
use luthor::{
    state::{AmendedDispatchProof, EffectiveConfigSnapshot, InitialBranchRemovalAudit},
    supervisor::{ExitReceipt, LaunchPlan},
};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{fs, process::Stdio};

fn launch_under_lock(case: &AmendmentFixture) -> std::process::Output {
    let f = &case.f;
    fs::write(f.dir.path().join("block"), "").unwrap();
    let child = case
        .command(&case.args())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&f.dir.path().join("observing"));
    assert!(StateStore::open(&f.config.state_root, 1).is_err());
    assert_eq!(f.count("evidence", "initial_branch_removed"), 0);
    let second = case.run(&case.args());
    assert!(!second.status.success());
    assert!(second.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&second.stderr),
        "luthor: amendment state unavailable\n"
    );
    fs::write(f.dir.path().join("release"), "").unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn amendment_cli_native_worker_receipt_and_replay_preserve_one_reserved_original_attempt() {
    let mut original = Vec::new();
    let mut config_bytes = Vec::new();
    let (case, out) = super::fresh_os_run(|case| {
        original = case
            .f
            .rows(&["tasks", "attempts", "reservations", "intents", "evidence"]);
        config_bytes = fs::read(&case.f.config_path).unwrap();
        launch_under_lock(case)
    });
    let f = &case.f;
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        result,
        json!({"command":"amend-undispatched", "task_id":f.task,
        "attempt_id":f.attempt,"config_revision":"saved-revision",
        "current_config_revision":"corrected-revision","status":"amended_dispatched"})
    );
    let receipt_path = f
        .config
        .state_root
        .join(format!("attempts/{}.receipt.json", f.attempt));
    wait_for(&receipt_path);
    let receipt: ExitReceipt = serde_json::from_slice(&fs::read(receipt_path).unwrap()).unwrap();
    assert_eq!(receipt.attempt_id, f.attempt);
    assert_eq!(receipt.exit_code, Some(0));
    assert_eq!(receipt.signal, None);
    assert!(receipt.stdout_bytes > 0);
    let effective = assert_audit_and_dispatch(&case);
    let expected = std::iter::once(effective.worktree.to_str().unwrap())
        .chain(effective.args.iter().map(String::as_str))
        .map(|v| format!("{v}\n"))
        .collect::<String>();
    assert_eq!(fs::read_to_string(&f.marker).unwrap(), expected);
    assert_eq!(fs::read(&f.config_path).unwrap(), config_bytes);
    let after = f.rows(&["tasks", "attempts", "reservations", "intents", "evidence"]);
    assert_eq!(&after[..3], &original[..3]);
    for table in 3..5 {
        assert!(original[table].iter().all(|row| after[table].contains(row)));
    }
    case.assert_reserved_original();
    assert_replay(&case);
    super::super::git(&f.plan.worktree, &["diff", "--exit-code"]);
}

fn assert_audit_and_dispatch(case: &AmendmentFixture) -> LaunchPlan {
    let f = &case.f;
    let audit: InitialBranchRemovalAudit = serde_json::from_str(
        &f.db()
            .query_row(
                "SELECT payload FROM evidence WHERE kind='initial_branch_removed'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(audit.saved_launch_plan, f.saved);
    assert_eq!(audit.original_plan, f.plan);
    assert_eq!(
        audit.current_config,
        EffectiveConfigSnapshot::from(&case.corrected)
    );
    assert_eq!(audit.current_config_revision, "corrected-revision");
    assert_eq!(audit.actor, "acoliver");
    assert_eq!(
        serde_json::to_value(audit.reason_code).unwrap(),
        "native_initial_branch_is_conversation"
    );
    let mut expected = f.plan.clone();
    expected.args.drain(2..4);
    assert_eq!(audit.effective_plan, expected);
    assert!(
        !expected
            .args
            .iter()
            .any(|arg| arg == "--branch" || arg.starts_with("--branch="))
    );
    let proof: AmendedDispatchProof = serde_json::from_str(
        &f.db()
            .query_row(
                "SELECT detail FROM intents WHERE kind='supervisor_dispatch'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(proof.effective_plan, expected);
    let seq: i64 = f
        .db()
        .query_row(
            "SELECT sequence FROM evidence WHERE kind='initial_branch_removed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(proof.amendment_sequence, seq);
    let plan: LaunchPlan = serde_json::from_slice(
        &fs::read(
            f.config
                .state_root
                .join(format!("attempts/{}.plan.json", f.attempt)),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(plan, expected);
    for kind in ["child_registered", "supervisor_ready", "gate_sent"] {
        assert_eq!(f.count("evidence", kind), 1);
    }
    assert_eq!(f.count("intents", "gate_release"), 1);
    expected
}

pub(super) fn assert_replay(case: &AmendmentFixture) {
    let f = &case.f;
    let before = f.rows(&["tasks", "attempts", "reservations", "intents", "evidence"]);
    let calls = fs::read(&f.calls).unwrap();
    let marker = fs::read(&f.marker).ok();
    case.held("ineligible");
    assert_eq!(
        f.rows(&["tasks", "attempts", "reservations", "intents", "evidence"]),
        before
    );
    assert_eq!(fs::read(&f.calls).unwrap(), calls);
    assert_eq!(fs::read(&f.marker).ok(), marker);
    assert_eq!(f.count("evidence", "initial_branch_removed"), 1);
    case.assert_reserved_original();
}

#[test]
fn amendment_cli_authenticates_and_exhaustively_reads_without_github_writes() {
    let (case, out) = super::fresh_os_run(|case| case.run(&case.args()));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    wait_for(
        &case
            .f
            .config
            .state_root
            .join(format!("attempts/{}.receipt.json", case.f.attempt)),
    );
    let calls = fs::read_to_string(&case.f.calls).unwrap();
    for prefix in ["api graphql", "api user"] {
        assert!(
            calls
                .lines()
                .filter(|line| line.starts_with(prefix))
                .count()
                >= 2
        );
    }
    assert!(calls.lines().filter(|line| line.contains("pulls?")).count() >= 2);
    assert!(!calls.contains(" -X "));
    assert!(!calls.contains("mutation"));
}

#[test]
fn amendment_cli_missing_namespace_is_created_private_only_after_consent() {
    let (case, out) = super::fresh_os_run(|case| {
        let namespace = case.f.config.state_root.join("attempts");
        fs::remove_dir(&namespace).unwrap();
        let mut args = case.args();
        args.pop();
        let refusal = case.run(&args);
        assert!(!refusal.status.success());
        assert!(!namespace.exists());
        assert_eq!(case.f.count("evidence", "initial_branch_removed"), 0);
        case.run(&case.args())
    });
    let namespace = case.f.config.state_root.join("attempts");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    wait_for(&namespace.join(format!("{}.receipt.json", case.f.attempt)));
    let metadata = fs::symlink_metadata(&namespace).unwrap();
    assert!(metadata.file_type().is_dir());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_audit_and_dispatch(&case);
    case.assert_reserved_original();
}
