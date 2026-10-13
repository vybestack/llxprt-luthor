use super::cleanup_fixture::{TerminalPr, close};
use super::*;
use luthor::state::{journal, task_records, worktree_records};
use serde_json::{Value, json};

pub(super) fn candidate(config: &Config, number: u64) -> Candidate {
    let (_, mut candidate) = configured(config.mappings[0].checkout.parent().unwrap());
    candidate.mapping = config.mappings[0].clone();
    candidate.item_id = format!("item-{number}");
    candidate.issue_node_id = format!("issue-{number}");
    candidate.issue_number = number;
    candidate.issue_url = format!("https://github.com/org/tracker/issues/{number}");
    candidate
}

pub(super) fn complete(store: &mut StateStore, config: &Config, task: &str, number: u64) -> Value {
    let candidate = candidate(config, number);
    task_records::create_task(store, task, &candidate, "rev", config).unwrap();
    task_records::record_claim_intent(store, task, "operator", "org/tracker", number).unwrap();
    journal::record_evidence(store, task, None, "claim_verified", "operator").unwrap();
    task_records::set_task_phase(store, task, "claimed").unwrap();
    ensure_worktree(store, task, &config.worktree_root, &candidate.mapping).unwrap();
    let attempt = format!("attempt-{task}");
    let plan = prepare_initial(store, task, &attempt).unwrap();
    let owner = luthor::WorktreeOwner::acquire(store.root(), task).unwrap();
    execute_with_binary(
        store,
        &plan,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    wait_for_exit(store, config, task, &attempt);
    let mut prs = terminal_pr(store, task, number);
    let result = luthor::coordinator::reconcile_with_pr(
        store,
        task,
        &attempt,
        &mut OtherProject(candidate, 1),
        &mut prs,
    )
    .unwrap();
    assert!(
        matches!(result, Reconciliation::Completed { .. }),
        "{result:?}"
    );
    assert_eq!(
        task_records::task_phase(store, task).unwrap().as_deref(),
        Some("pr_complete")
    );
    close(&mut prs, true);
    prs.detail
}

fn terminal_pr(store: &StateStore, task: &str, number: u64) -> TerminalPr {
    let identity = worktree_records::worktree_record(store, task)
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    TerminalPr {
        login: "acoliver".into(),
        reads: 0,
        replacement: false,
        fail: false,
        closed: false,
        detail: json!({
            "id":4200+number,"number":number,"state":"open","merged":false,
            "html_url":format!("https://github.com/org/code/pull/{number}"),
            "body":format!("Tracker-Issue: https://github.com/org/tracker/issues/{number}\nFixes org/tracker#{number}"),
            "draft":false,"created_at":"2026-01-01T00:00:00Z",
            "base":{"repo":{"id":10,"full_name":"org/code"},"ref":"main"},
            "head":{"repo":{"id":10,"full_name":"org/code"},"ref":identity.branch,"sha":identity.head},
            "user":{"login":"acoliver"},"closed_at":null,"merged_at":null
        }),
    }
}

fn wait_for_exit(store: &StateStore, config: &Config, task: &str, attempt: &str) {
    let path = config
        .state_root
        .join(format!("attempts/{attempt}.receipt.json"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            !config
                .state_root
                .join(format!("attempts/{attempt}.supervisor-error.json"))
                .exists()
        );
        assert!(Instant::now() < deadline, "fixture receipt missing");
        thread::sleep(Duration::from_millis(10));
    }
    let ready = journal::evidence_payloads(store, task, attempt, "supervisor_ready").unwrap();
    assert_eq!(ready.len(), 1);
    let ready: Value = serde_json::from_str(&ready[0]).unwrap();
    wait_for_process_and_group_absence(
        libc::pid_t::try_from(ready["pid"].as_u64().unwrap()).unwrap(),
    );
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    wait_for_process_and_group_absence(libc::pid_t::try_from(receipt.child_pid).unwrap());
    crate::supervisor_support::wait_for_worktree_owner_release(store.root(), task);
}

pub(super) fn config(dir: &Path) -> Config {
    let (mut config, _) = configured(dir);
    config.mappings[0].allowed_pr_author = "acoliver".into();
    config.initial.executable = "/usr/bin/true".into();
    config.resume.executable = "/usr/bin/true".into();
    config.cleanup = luthor::config::CleanupPolicy::Worktree;
    let checkout = &config.mappings[0].checkout;
    fs::create_dir(checkout).unwrap();
    git(checkout, &["init", "-b", "main"]);
    git(checkout, &["config", "user.name", "Fixture"]);
    git(checkout, &["config", "user.email", "fixture@example.org"]);
    git(
        checkout,
        &["remote", "add", "origin", "git@github.com:org/code.git"],
    );
    fs::write(checkout.join("README"), "fixture").unwrap();
    fs::write(checkout.join(".gitignore"), "tmp/\ntarget/\n").unwrap();
    git(checkout, &["add", "README", ".gitignore"]);
    git(checkout, &["commit", "-m", "base"]);
    crate::git_fixture::remote(checkout);
    config
}
