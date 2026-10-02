use super::*;

#[cfg(unix)]
pub(crate) fn retry_natural_exit_audits_current_argv_preserves_history_and_reconciles_new_revision()
{
    let (_dir, mut config, mut store) = retry_fixture();
    let original = store.selection_evidence("task").unwrap().unwrap();
    let old_plan = store.launch_intent("attempt-real").unwrap().unwrap();
    let old_receipt = fs::read(receipt_path(&config)).unwrap();
    let old_rows = old_retry_rows(&config);
    config
        .resume
        .args
        .extend(["--max-tool-calls".into(), "512".into()]);
    let mut prs = ExitPr::default();
    let mut projects = OtherProject(original.candidate.clone(), 1);
    let mut launcher = OtherLauncher::default();
    let plan = retry(&mut store, &config, &mut projects, &mut prs, &mut launcher).unwrap();
    assert_eq!(launcher.0, 1);
    assert_eq!(
        prs.reads, 1,
        "cached PR absence must not authorize continuation"
    );
    assert_eq!(plan.config_revision, "corrected");
    assert_eq!(plan.session_id, "task");
    assert_eq!(plan.expected_worktree.branch, "luthor/task");
    assert!(
        plan.args
            .windows(2)
            .any(|pair| pair == ["--max-tool-calls", "512"])
    );
    let prompt = plan.args.windows(2).find(|p| p[0] == "--prompt").unwrap()[1].as_str();
    assert!(prompt.contains("naturally exited worker"));
    assert!(!prompt.contains("interrupted or canceled turn"));
    assert_eq!(store.selection_evidence("task").unwrap().unwrap(), original);
    assert_eq!(
        store.launch_intent("attempt-real").unwrap().unwrap(),
        old_plan
    );
    assert_eq!(fs::read(receipt_path(&config)).unwrap(), old_receipt);
    assert_eq!(store.reservation_count().unwrap(), 1);
    let audit = store
        .evidence_payloads("task", "attempt-retry", "retry_authorized")
        .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&audit[0]).unwrap();
    assert_eq!(audit["actor"], "operator");
    assert_eq!(audit["reason"], "correct worker budget");
    assert_eq!(audit["previous_plan"]["config_revision"], "rev");
    assert_eq!(audit["plan"]["attempt_id"], "attempt-retry");
    assert_eq!(audit["reservation"], "attempt-retry");
    assert_eq!(audit["pr"]["status"]["status"], "absent");
    assert!(retry(&mut store, &config, &mut projects, &mut prs, &mut launcher).is_err());
    assert_eq!(launcher.0, 1);
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    wait_retry_receipt(&config);
    assert_exact_retry_audit_required(&config, &mut store);
    assert!(matches!(
        luthor::coordinator::reconcile_with_pr(
            &mut store,
            "task",
            "attempt-retry",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        Reconciliation::Completed {
            exit_code: Some(2),
            signal: None
        }
    ));
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert_eq!(old_retry_rows(&config), old_rows);
    assert_eq!(
        store.task_phase("task").unwrap().as_deref(),
        Some("attention")
    );
    assert_eq!(prs.reads, 2);
    assert_eq!(store.selection_evidence("task").unwrap().unwrap(), original);
    assert!(prepare_resume(&mut store, "task", "forbidden-resume").is_err());
    assert_eq!(fs::read_dir(&config.worktree_root).unwrap().count(), 1);
}

#[cfg(unix)]
pub(crate) fn retry_refusals_never_reserve_or_launch_or_rewrite_selection() {
    for historical in [false, true] {
        retry_refusals_with_history(historical);
    }
}

#[cfg(unix)]
pub(crate) fn retry_unsupported_saved_budget_uses_corrected_template_without_rewriting_old_argv() {
    let (_dir, mut config, mut store) = retry_fixture_with_saved_budget(Some("1024"));
    let original = store.selection_evidence("task").unwrap().unwrap();
    let old_plan = store.launch_intent("attempt-real").unwrap().unwrap();
    let old_plan_typed: luthor::supervisor::LaunchPlan = serde_json::from_str(&old_plan).unwrap();
    assert!(
        old_plan_typed
            .args
            .windows(2)
            .any(|p| p == ["--max-tool-calls", "1024"])
    );
    config
        .initial
        .args
        .extend(["--max-tool-calls".into(), "512".into()]);
    config
        .resume
        .args
        .extend(["--max-tool-calls".into(), "512".into()]);
    let mut launcher = OtherLauncher::default();
    let plan = retry(
        &mut store,
        &config,
        &mut OtherProject(original.candidate.clone(), 1),
        &mut ExitPr::default(),
        &mut launcher,
    )
    .unwrap();
    assert!(
        plan.args
            .windows(2)
            .any(|p| p == ["--max-tool-calls", "512"])
    );
    assert!(!plan.args.iter().any(|arg| arg == "1024"));
    assert_eq!(launcher.0, 1);
    assert_eq!(store.selection_evidence("task").unwrap().unwrap(), original);
    assert_eq!(
        store.launch_intent("attempt-real").unwrap().unwrap(),
        old_plan
    );
}

fn wait_retry_receipt(config: &Config) {
    let path = config
        .state_root
        .join("attempts/attempt-retry.receipt.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "authorized new revision did not pass worker gate"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn assert_exact_retry_audit_required(config: &Config, store: &mut StateStore) {
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let audit: String = db.query_row("SELECT payload FROM evidence WHERE attempt_id='attempt-retry' AND kind='retry_authorized'", [], |row| row.get(0)).unwrap();
    let phase = store.task_phase("task").unwrap();
    for mismatch in [false, true] {
        if mismatch {
            let mut changed: serde_json::Value = serde_json::from_str(&audit).unwrap();
            changed["plan"]["config_revision"] = "unapproved".into();
            db.execute("UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-retry' AND kind='retry_authorized'", [changed.to_string()]).unwrap();
        } else {
            db.execute(
                "DELETE FROM evidence WHERE attempt_id='attempt-retry' AND kind='retry_authorized'",
                [],
            )
            .unwrap();
        }
        let rows = old_retry_rows(config);
        assert!(
            matches!(reconcile_attempt(store, "task", "attempt-retry").unwrap(), Reconciliation::Held { reason } if reason == "selection mismatch")
        );
        assert_eq!(store.reservation_count().unwrap(), 1);
        assert_eq!(old_retry_rows(config), rows);
        assert_eq!(
            store.task_phase("task").unwrap().as_deref(),
            phase.as_deref()
        );
        if mismatch {
            db.execute("UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-retry' AND kind='retry_authorized'", [&audit]).unwrap();
        } else {
            db.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','attempt-retry','retry_authorized',?1)", [&audit]).unwrap();
        }
    }
}
