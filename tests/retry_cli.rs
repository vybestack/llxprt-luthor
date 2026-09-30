#![cfg(unix)]
use luthor::{config::Config, state::StateStore, supervisor::Reconciliation};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn run(config: &Path, path: &str, command: &str, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_luthor"))
        .arg(command)
        .args(extra)
        .arg("--config")
        .arg(config)
        .env("PATH", path)
        .output()
        .unwrap()
}

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn explicit_retry_cli_uses_private_config_and_never_reassigns_or_reselects() {
    let dir = tempfile::Builder::new()
        .prefix("lr")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path();
    let checkout = root.join("checkout");
    fs::create_dir(&checkout).unwrap();
    git(&checkout, &["init", "-b", "main"]);
    git(&checkout, &["config", "user.name", "Fixture"]);
    git(&checkout, &["config", "user.email", "fixture@example.org"]);
    git(
        &checkout,
        &["remote", "add", "origin", "git@github.com:org/code.git"],
    );
    fs::write(checkout.join("README"), "fixture").unwrap();
    git(&checkout, &["add", "README"]);
    git(&checkout, &["commit", "-m", "initial"]);
    let issue = |assigned| {
        json!({"node_id":"issue", "number":7,
        "repository_url":"https://api.github.com/repos/org/tracker",
        "html_url":"https://github.com/org/tracker/issues/7", "state":"open",
        "assignees": if assigned {vec![json!({"login":"acoliver"})]} else {vec![]},
        "labels":[{"name":"ready"}], "milestone":null})
    };
    let project = json!({"data":{"node":{"items":{"nodes":[{
        "id":"item", "content":{"__typename":"Issue", "id":"issue", "number":7,
        "repository":{"id":"repo", "nameWithOwner":"org/tracker"}},
        "fieldValues":{"nodes":[], "pageInfo":{"hasNextPage":false}}
    }],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}});
    let gh = root.join("gh");
    let calls = root.join("calls");
    let assigned = root.join("assigned");
    fs::write(&gh, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$2\" in\n graphql) printf '%s\\n' '{}' ;;\n repos/org/tracker) printf '%s\\n' '{{\"node_id\":\"repo\"}}' ;;\n repos/org/tracker/issues/7?per_page=100) if test -f '{}'; then printf '%s\\n' '{}'; else printf '%s\\n' '{}'; fi ;;\n -X) touch '{}'; printf '%s\\n' '{{}}' ;;\n repos/org/code/pulls?*) printf '%s\\n' '[]' ;;\n user) printf '%s\\n' 'acoliver' ;;\n *) exit 99 ;;\nesac\n", calls.display(), project, assigned.display(), issue(true), issue(false), assigned.display())).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = root.join("worker");
    fs::write(&worker, "#!/bin/sh\nexit 2\n").unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
    let command = |prompt| json!({"executable":worker,"args":["--session","{task.id}","--cwd","{worktree}","-p",prompt,"--max-tool-calls","1"]});
    let config_value = json!({"state_root":root.join("state"), "worktree_root":root.join("worktrees"), "capacity":1,
        "assignment_login":"acoliver", "sources":[{"project_id":"project","repositories":["org/tracker"],
            "ready_marker":{"kind":"label","name":"ready"},"milestone":null}],
        "mappings":[{"tracker_repository":"org/tracker","code_repository":"org/code", "checkout":checkout,
            "base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/code","allowed_pr_author":"acoliver"}],
        "initial":command("Start {task.issue_url}"), "resume":command("Continue {task.issue_url} for {attempt.id}")});
    let config_path = root.join("config.json");
    fs::write(&config_path, config_value.to_string()).unwrap();
    let path = format!("{}:{}", root.display(), std::env::var("PATH").unwrap());
    let out = run(
        &config_path,
        &path,
        "dispatch",
        &[
            "--repository",
            "org/tracker",
            "--issue",
            "7",
            "--config-revision",
            "old",
            "--execute",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dispatched: Value = serde_json::from_slice(&out.stdout).unwrap();
    let task = dispatched["task_id"].as_str().unwrap();
    let previous = dispatched["attempt_id"].as_str().unwrap();
    let config = Config::from_json(&config_value.to_string()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !config
        .state_root
        .join(format!("attempts/{previous}.receipt.json"))
        .exists()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let out = run(&config_path, &path, "reconcile", &[task]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let store = StateStore::open(&config.state_root, 1).unwrap();
    assert_eq!(
        store.task_phase(task).unwrap().as_deref(),
        Some("attention")
    );
    let original = store.selection_evidence(task).unwrap().unwrap();
    let prior_plan = store.launch_intent(previous).unwrap().unwrap();
    let ready = rusqlite::Connection::open(config.state_root.join("state.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT payload FROM evidence WHERE task_id=?1 AND kind='supervisor_ready'",
            [task],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    let ready: Value = serde_json::from_str(&ready).unwrap();
    let pid = ready["pid"].as_i64().unwrap() as i32;
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    drop(store);
    let mut corrected = config.clone();
    *corrected.initial.args.last_mut().unwrap() = "512".into();
    *corrected.resume.args.last_mut().unwrap() = "512".into();
    fs::write(&config_path, serde_json::to_string(&corrected).unwrap()).unwrap();
    let invoke = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_luthor"))
            .args([
                "retry",
                task,
                "--attempt",
                previous,
                "--config",
                config_path.to_str().unwrap(),
                "--config-revision",
                "corrected-512",
                "--actor",
                "acoliver",
                "--reason",
                "correct launch budget",
            ])
            .args(extra)
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    let before = fs::read_to_string(&calls).unwrap();
    assert!(!invoke(&[]).status.success());
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        before,
        "missing execution authorization performed GitHub reads"
    );
    let out = invoke(&["--execute"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let retried: Value = serde_json::from_slice(&out.stdout).unwrap();
    let attempt = retried["attempt_id"].as_str().unwrap();
    assert_eq!(retried["status"], "retried");
    assert_eq!(retried["config_revision"], "corrected-512");
    assert_ne!(attempt, previous);
    assert!(!invoke(&["--execute"]).status.success());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    assert_eq!(store.selection_evidence(task).unwrap().unwrap(), original);
    assert_eq!(store.launch_intent(previous).unwrap().unwrap(), prior_plan);
    let new_plan: luthor::supervisor::LaunchPlan =
        serde_json::from_str(&store.launch_intent(attempt).unwrap().unwrap()).unwrap();
    assert!(
        new_plan
            .args
            .windows(2)
            .any(|p| p == ["--max-tool-calls", "512"])
    );
    assert_eq!(original.effective_config.resume.args.last().unwrap(), "1");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !config
        .state_root
        .join(format!("attempts/{attempt}.receipt.json"))
        .exists()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(
        luthor::supervisor::reconcile_attempt(&mut store, task, attempt).unwrap(),
        Reconciliation::Completed {
            exit_code: Some(2),
            signal: None
        }
    ));
    assert_eq!(
        fs::read_to_string(calls)
            .unwrap()
            .lines()
            .filter(|l| l.contains(" -X "))
            .count(),
        1
    );
}
