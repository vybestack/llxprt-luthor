use super::*;

#[cfg(unix)]
pub(crate) fn retry_refusals_with_history(historical: bool) {
    for refusal in [
        "receipt",
        "plan",
        "outcome",
        "stop",
        "active",
        "reserved",
        "incomplete",
        "receipt_corrupt",
        "source",
        "mapping",
        "identity",
        "worktree",
        "capacity",
        "pr_error",
        "pr_present",
        "pr_ambiguous",
        "pr_identity",
        "task_identity",
        "claim_intent",
        "tracked_live",
        "tracked_reused",
        "supervisor_contradiction",
        "receipt_contradiction",
        "supervisor_error",
        "invalid_config",
    ] {
        let (_dir, mut config, mut store) = if historical {
            historical_retry_fixture()
        } else {
            retry_fixture()
        };
        let original = store.selection_evidence("task").unwrap().unwrap();
        let old_plan = store.launch_intent("attempt-real").unwrap().unwrap();
        let mut projects = OtherProject(original.candidate.clone(), 1);
        let mut prs = ExitPr::default();
        let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
        apply_refusal(
            refusal,
            &mut config,
            &mut store,
            &original,
            &mut projects,
            &mut prs,
            &db,
        );
        assert_refused_retry(
            historical,
            refusal,
            &config,
            &mut store,
            (&original, old_plan.as_str()),
            &mut projects,
            &mut prs,
        );
    }
}

fn apply_refusal(
    refusal: &str,
    config: &mut Config,
    store: &mut StateStore,
    original: &luthor::state::SelectionEvidence,
    projects: &mut OtherProject,
    prs: &mut ExitPr,
    db: &rusqlite::Connection,
) {
    match refusal {
        "receipt" => {
            fs::remove_file(receipt_path(config)).unwrap();
        }
        "plan" => {
            fs::write(
                config.state_root.join("attempts/attempt-real.plan.json"),
                "{}",
            )
            .unwrap();
        }
        "outcome" => {
            db.execute(
                "UPDATE attempts SET outcome='wrong' WHERE id='attempt-real'",
                [],
            )
            .unwrap();
        }
        "stop" => {
            store
                .record_intent("late-stop", "task", Some("attempt-real"), "stop", "{}")
                .unwrap();
        }
        "active" => {
            store.set_task_phase("task", "held").unwrap();
        }
        "reserved" => {
            db.execute(
                "UPDATE reservations SET status='reserved' WHERE attempt_id='attempt-real'",
                [],
            )
            .unwrap();
        }
        "incomplete" => {
            db.execute("UPDATE attempts SET lifecycle='launch_intended',outcome=NULL WHERE id='attempt-real'", []).unwrap();
        }
        "receipt_corrupt" => {
            fs::write(receipt_path(config), "{").unwrap();
        }
        "source" | "mapping" | "identity" | "worktree" | "capacity" | "pr_error" | "pr_present"
        | "pr_ambiguous" | "pr_identity" => {
            apply_source_refusal(refusal, config, store, original, projects, prs)
        }
        "task_identity" => {
            db.execute(
                "UPDATE tasks SET tracker_repo_id='different' WHERE id='task'",
                [],
            )
            .unwrap();
        }
        "claim_intent" => {
            db.execute(
                "UPDATE intents SET detail='{}' WHERE task_id='task' AND kind='claim_assignment'",
                [],
            )
            .unwrap();
        }
        "tracked_live"
        | "tracked_reused"
        | "supervisor_contradiction"
        | "receipt_contradiction"
        | "supervisor_error" => apply_process_refusal(refusal, config, store, db),
        "invalid_config" => {
            config
                .resume
                .args
                .extend(["--max-tool-calls".into(), "1024".into()]);
        }
        _ => unreachable!(),
    }
}

fn apply_source_refusal(
    refusal: &str,
    config: &mut Config,
    store: &mut StateStore,
    original: &luthor::state::SelectionEvidence,
    projects: &mut OtherProject,
    prs: &mut ExitPr,
) {
    match refusal {
        "source" => {
            projects.1 = 0;
        }
        "mapping" => {
            config.mappings[0].base_branch = "other".into();
        }
        "identity" => {
            config.assignment_login = "another".into();
        }
        "worktree" => {
            git(
                &config.worktree_root.join("task"),
                &["checkout", "--detach"],
            );
        }
        "capacity" => {
            let mut other = original.candidate.clone();
            other.issue_node_id = "other".into();
            other.item_id = "other".into();
            other.issue_number = 8;
            other.issue_url = "https://github.com/org/tracker/issues/8".into();
            store.create_task("other", &other, "rev", config).unwrap();
            store.reserve("other", "other-attempt").unwrap();
        }
        "pr_error" => {
            prs.fail = true;
        }
        "pr_present" => {
            prs.matching = Some((original.candidate.issue_url.clone(), "luthor/task".into()));
        }
        "pr_ambiguous" => {
            prs.ambiguous = true;
            prs.matching = Some((original.candidate.issue_url.clone(), "luthor/task".into()));
        }
        "pr_identity" => {
            prs.login = Some("another".into());
        }
        _ => unreachable!(),
    }
}

fn assert_refused_retry(
    historical: bool,
    refusal: &str,
    config: &Config,
    store: &mut StateStore,
    history: (&luthor::state::SelectionEvidence, &str),
    projects: &mut OtherProject,
    prs: &mut ExitPr,
) {
    let (original, old_plan) = history;
    let reservations = store.reservation_count().unwrap();
    let rows = old_retry_rows(config);
    let mut launcher = OtherLauncher::default();
    assert!(
        (if historical {
            historical_retry(store, config, projects, prs, &mut launcher)
        } else {
            retry(store, config, projects, prs, &mut launcher)
        })
        .is_err(),
        "{refusal}"
    );
    assert_eq!(launcher.0, 0, "{refusal}");
    assert_eq!(
        old_retry_rows(config),
        rows,
        "refusal changed old history: {refusal}, historical={historical}"
    );
    assert!(
        store
            .evidence_payloads("task", "attempt-retry", "retry_authorized")
            .unwrap()
            .is_empty(),
        "{refusal}"
    );
    assert_eq!(
        store.reservation_count().unwrap(),
        reservations,
        "{refusal}"
    );
    assert_eq!(
        store.latest_attempt("task").unwrap().as_deref(),
        Some("attempt-real"),
        "{refusal}"
    );
    assert_eq!(
        store.selection_evidence("task").unwrap().unwrap(),
        *original,
        "{refusal}"
    );
    assert_eq!(
        store.launch_intent("attempt-real").unwrap().unwrap(),
        old_plan,
        "{refusal}"
    );
    assert!(
        store.launch_intent("attempt-retry").unwrap().is_none(),
        "{refusal}"
    );
}

fn apply_process_refusal(
    refusal: &str,
    config: &Config,
    store: &mut StateStore,
    db: &rusqlite::Connection,
) {
    match refusal {
        "tracked_live" | "tracked_reused" => {
            let (boot, mut start) = test_process_identity(std::process::id());
            if refusal == "tracked_reused" {
                start = "different historical start".into();
            }
            store.record_evidence("task", Some("attempt-real"), "tracked_descendant", &serde_json::json!({"pid":std::process::id(), "boot_identity":boot,"start_identity":start}).to_string()).unwrap();
        }
        "supervisor_contradiction" => {
            db.execute("UPDATE evidence SET payload=json_set(payload,'$.boot_identity','contradiction') WHERE attempt_id='attempt-real' AND kind='supervisor_ready'", []).unwrap();
        }
        "receipt_contradiction" => {
            edit_receipt(config, |receipt| {
                receipt.boot_identity = "contradiction".into()
            });
        }
        "supervisor_error" => {
            fs::write(
                config
                    .state_root
                    .join("attempts/attempt-real.supervisor-error.json"),
                "{}",
            )
            .unwrap();
        }
        _ => unreachable!(),
    }
}
