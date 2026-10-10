use luthor::{
    cli::{CliError, execute},
    state::StateStore,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn seed(root: &Path) -> Connection {
    let _store = StateStore::open(root, 1).unwrap();
    let db = Connection::open(root.join("state.sqlite3")).unwrap();
    db.execute("INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision) VALUES('task','repo','issue','org/tracker',7,'held','rev')", []).unwrap();
    db.execute(
        "INSERT INTO attempts(id,task_id,lifecycle) VALUES('attempt','task','launch_intended')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO reservations(attempt_id,task_id,status) VALUES('attempt','task','reserved')",
        [],
    )
    .unwrap();
    db
}

fn view(root: &Path, command: &str) -> Result<Value, CliError> {
    let mut args = vec![command.to_owned()];
    if command == "show" {
        args.push("task".into());
    }
    let output: Value = serde_json::from_str(&execute(root, &args)?).unwrap();
    Ok(if command == "status" {
        output["tasks"][0].clone()
    } else {
        output
    })
}

#[test]
fn status_uses_attempt_age_but_show_requires_verified_log_output() {
    let dir = tempfile::tempdir().unwrap();
    let db = seed(dir.path());
    for (created, expected_status, warning) in [
        ("2000-01-01 00:00:00", "no_output_yet", true),
        ("2999-01-01 00:00:00", "unavailable", false),
        ("invalid", "unavailable", false),
    ] {
        db.execute("UPDATE attempts SET created_at=?1", [created])
            .unwrap();
        let status = view(dir.path(), "status").unwrap();
        let shown = view(dir.path(), "show").unwrap();
        assert_eq!(status["output_log_status"], expected_status);
        assert_eq!(status["output_silence_warning"], warning);
        assert_eq!(status["last_output_age_seconds"].is_number(), warning);
        assert_eq!(shown["output_log_status"], "unavailable");
        assert_eq!(shown["last_output_age_seconds"], Value::Null);
        assert_eq!(
            shown["output_age_unavailable_reason"],
            "no verified output log data"
        );
        for output in [status, shown] {
            assert_eq!(output["phase"], "held");
            assert_eq!(output["process"], Value::Null);
            assert_eq!(output["reserved_slot"], true);
            assert_eq!(output["observed_stdout_bytes"], Value::Null);
            assert_eq!(output["observed_stderr_bytes"], Value::Null);
        }
    }
    db.execute("UPDATE reservations SET status='released'", [])
        .unwrap();
    for command in ["status", "show"] {
        let output = view(dir.path(), command).unwrap();
        assert_eq!(
            output["output_age_unavailable_reason"],
            "no reserved attempt"
        );
        assert_eq!(
            output["observed_bytes_unavailable_reason"],
            "no reserved attempt"
        );
        assert_eq!(output["output_silence_warning"], false);
    }
}

fn launch_plan() -> Value {
    json!({"task_id":"task","attempt_id":"attempt","session_id":"persisted-session",
        "worktree":"/worktree","expected_worktree":{"path":"/worktree","device":1,"inode":1,
        "branch":"luthor/task","base":"main","head":"abc","repository":"org/code",
        "git_directory":"/checkout/.git","remote":"origin"},"executable":"/worker",
        "args":["private-prompt"],"config_revision":"rev",
        "session_environment":{"home":"/","xdg_config_home":null,"xdg_data_home":null,
        "xdg_state_home":null,"llxprt_config_home":null}})
}

#[test]
fn show_validates_persisted_launch_identity_without_requiring_task_named_session() {
    let dir = tempfile::tempdir().unwrap();
    let db = seed(dir.path());
    let plan = launch_plan();
    db.execute("INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('launch','task','attempt','launch',?1)", [plan.to_string()]).unwrap();
    let shown = view(dir.path(), "show").unwrap();
    assert_eq!(shown["session"], "persisted-session");
    assert!(!shown.to_string().contains("private-prompt"));
    for key in ["task_id", "attempt_id"] {
        let mut wrong = plan.clone();
        wrong[key] = json!("foreign");
        db.execute("UPDATE intents SET detail=?1", [wrong.to_string()])
            .unwrap();
        assert_eq!(view(dir.path(), "show"), Err(CliError::Database));
    }
    db.execute("UPDATE intents SET detail='malformed'", [])
        .unwrap();
    assert_eq!(view(dir.path(), "show"), Err(CliError::Database));
}

#[test]
fn controls_reject_invalid_arguments_before_configuration_access() {
    for (args, error) in [
        (vec!["pause"], "task id is required"),
        (
            vec!["reconcile", "task", "--attempt"],
            "attempt id is required",
        ),
        (
            vec![
                "pause",
                "task",
                "--attempt",
                "id",
                "--config",
                "/must/not/open",
            ],
            "expected TASK",
        ),
        (
            vec!["reconcile", "task", "--config", "--bad"],
            "expected TASK",
        ),
        (
            vec!["pause", "task", "--config", "/must/not/open", "extra"],
            "expected TASK",
        ),
        (
            vec![
                "reconcile",
                "task",
                "--attempt",
                "id",
                "--attempt",
                "id",
                "--config",
                "/must/not/open",
            ],
            "expected TASK",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_luthor"))
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{args:?}: {:?}",
            output.stderr
        );
    }
}

#[test]
fn controls_accept_config_syntax_before_failing_at_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.json");
    for args in [
        vec!["pause", "task"],
        vec!["reconcile", "task"],
        vec!["reconcile", "task", "--attempt", "attempt"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_luthor"))
            .args(args)
            .arg("--config")
            .arg(&missing)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("No such file"));
    }
}

#[test]
fn attempt_history_retains_creation_order_and_validates_every_persisted_plan() {
    let dir = tempfile::tempdir().unwrap();
    let db = seed(dir.path());
    db.execute("UPDATE attempts SET created_at='2001-01-01 00:00:00'", [])
        .unwrap();
    db.execute("INSERT INTO attempts(id,task_id,lifecycle,created_at) VALUES('older','task','launch_intended','2000-01-01 00:00:00')", []).unwrap();
    let mut older = launch_plan();
    older["attempt_id"] = json!("older");
    older["session_id"] = json!("older-session");
    db.execute("INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('old-launch','task','older','launch',?1)", [older.to_string()]).unwrap();
    let shown = view(dir.path(), "show").unwrap();
    assert_eq!(shown["attempts"][0]["id"], "older");
    assert_eq!(shown["attempts"][1]["id"], "attempt");
    assert_eq!(shown["latest_attempt_id"], "attempt");
    assert_eq!(shown["session"], "older-session");
    let plan = launch_plan();
    db.execute("INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('launch','task','attempt','launch',?1)", [plan.to_string()]).unwrap();
    assert_eq!(
        view(dir.path(), "show").unwrap()["session"],
        "persisted-session"
    );
    db.execute(
        "UPDATE intents SET detail='malformed' WHERE id='old-launch'",
        [],
    )
    .unwrap();
    assert_eq!(view(dir.path(), "show"), Err(CliError::Database));
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM intents", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row("SELECT status FROM reservations", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "reserved"
    );
}

#[cfg(unix)]
#[test]
fn log_age_can_be_available_while_stream_byte_counts_fail_closed() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let _db = seed(dir.path());
    let attempts = dir.path().join("attempts");
    fs::create_dir(&attempts).unwrap();
    let stdout = attempts.join("attempt.stdout.log");
    let stderr = attempts.join("attempt.stderr.log");
    fs::write(&stdout, "private-output").unwrap();
    fs::set_permissions(&stdout, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&stdout, &stderr).unwrap();
    for command in ["status", "show"] {
        let output = view(dir.path(), command).unwrap();
        assert_eq!(output["output_log_status"], "available");
        assert!(output["last_output_age_seconds"].is_number());
        assert_eq!(output["output_age_unavailable_reason"], Value::Null);
        assert_eq!(output["observed_stdout_bytes"], Value::Null);
        assert_eq!(output["observed_stderr_bytes"], Value::Null);
        assert_eq!(output["observed_at_utc"], Value::Null);
        assert_eq!(
            output["observed_bytes_unavailable_reason"],
            "active attempt log files are missing or unsafe"
        );
        assert_eq!(output["byte_counts_are_observational"], true);
        assert!(!output.to_string().contains("private-output"));
    }
}
