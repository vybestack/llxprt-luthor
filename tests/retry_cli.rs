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

fn replace_fixture_boot(state: &Path, attempt: &str, boot: &str) {
    let receipt_path = state.join(format!("attempts/{attempt}.receipt.json"));
    let mut receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    receipt.boot_identity = boot.into();
    fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let child_path = state.join(format!("attempts/{attempt}.child.json"));
    let mut child: Value = serde_json::from_slice(&fs::read(&child_path).unwrap()).unwrap();
    child["boot_identity"] = json!(boot);
    fs::write(&child_path, serde_json::to_vec(&child).unwrap()).unwrap();
    let db = rusqlite::Connection::open(state.join("state.sqlite3")).unwrap();
    db.execute("UPDATE evidence SET payload=json_set(payload,'$.boot_identity',?1)
        WHERE attempt_id=?2 AND kind IN ('child_registered','supervisor_ready','gate_sent','tracked_descendant')",
        [boot, attempt]).unwrap();
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE attempt_id=?2 AND kind='attempt_exit'",
        [serde_json::to_string(&receipt).unwrap().as_str(), attempt],
    )
    .unwrap();
    db.execute(
        "UPDATE intents SET detail=json_set(detail,'$.boot_identity',?1)
        WHERE attempt_id=?2 AND kind='gate_release'",
        [boot, attempt],
    )
    .unwrap();
}

fn state_rows(state: &Path) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let db = rusqlite::Connection::open(state.join("state.sqlite3")).unwrap();
    [
        "tasks",
        "attempts",
        "reservations",
        "intents",
        "evidence",
        "state_meta",
    ]
    .iter()
    .map(|table| {
        let mut statement = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    })
    .collect()
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
    let receipt_file = config
        .state_root
        .join(format!("attempts/{previous}.receipt.json"));
    let initial_receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(&receipt_file).unwrap()).unwrap();
    #[cfg(target_os = "macos")]
    {
        let boot = Command::new("/usr/sbin/sysctl")
            .args(["-n", "kern.bootsessionuuid"])
            .output()
            .unwrap();
        assert!(boot.status.success());
        assert_eq!(
            initial_receipt.boot_identity,
            format!(
                "darwin-bootsessionuuid:{}",
                String::from_utf8(boot.stdout)
                    .unwrap()
                    .trim()
                    .to_ascii_lowercase()
            )
        );
    }
    for (boot, reason) in [
        (
            "{ sec = 1790533213, usec = 116017 } Sun Sep 27 15:20:13 2026",
            "historical Darwin boot identity cannot prove boot continuity",
        ),
        (
            "{ sec = 1790533213, usec = 220969 } Sun Sep 27 15:20:13 2026",
            "historical Darwin boot identity cannot prove boot continuity",
        ),
        (
            "darwin-bootsessionuuid:00000001-0000-4000-8000-000000000001",
            "registered boot identity differs from current boot",
        ),
        (
            "untrusted-secret\n\x1b[31m",
            "registered boot identity differs from current boot",
        ),
        ("", "child registration mismatch"),
    ] {
        // Only disposable fixture evidence is changed to reproduce historical records.
        replace_fixture_boot(&config.state_root, previous, boot);
        let rows = state_rows(&config.state_root);
        let files: Vec<_> = fs::read_dir(config.state_root.join("attempts"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p.is_file())
            .map(|p| {
                let bytes = fs::read(&p).unwrap();
                (p, bytes)
            })
            .collect();
        let calls_before = fs::read_to_string(&calls).unwrap();
        let out = invoke(&["--execute"]);
        assert!(!out.status.success(), "{boot}");
        assert!(out.stdout.is_empty());
        assert_eq!(
            String::from_utf8(out.stderr).unwrap(),
            format!("luthor: retry refused or held: {reason}\n")
        );
        assert_eq!(
            state_rows(&config.state_root),
            rows,
            "refusal changed history"
        );
        for (file, bytes) in files {
            assert_eq!(fs::read(file).unwrap(), bytes);
        }
        let calls_after = fs::read_to_string(&calls).unwrap();
        assert!(
            calls_after
                .strip_prefix(&calls_before)
                .unwrap()
                .lines()
                .all(|call| call.contains("user")),
            "refusal read source or PRs"
        );
    }
    replace_fixture_boot(&config.state_root, previous, &initial_receipt.boot_identity);
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
