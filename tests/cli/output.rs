use super::support::Fixture;
use serde_json::Value;
use std::fs;

pub(crate) fn status_and_show_report_missing_or_unsafe_active_logs_as_unavailable() {
    let f = Fixture::new();
    f.task("task");
    f.attempt("task", "attempt", "session");

    let status = f.run(&["status"]).unwrap();
    let task = &status["tasks"][0];
    assert_eq!(task["observed_stdout_bytes"], Value::Null);
    assert_eq!(task["observed_stderr_bytes"], Value::Null);
    assert_eq!(
        task["observed_bytes_unavailable_reason"],
        "active attempt log files are missing or unsafe"
    );
    assert_eq!(task["byte_counts_are_observational"], true);

    let shown = f.run(&["show", "task"]).unwrap();
    assert_eq!(shown["observed_stdout_bytes"], Value::Null);
    assert_eq!(shown["observed_stderr_bytes"], Value::Null);
    assert_eq!(
        shown["observed_bytes_unavailable_reason"],
        "active attempt log files are missing or unsafe"
    );

    let (stdout, stderr) = f.logs("attempt");
    #[cfg(unix)]
    {
        fs::remove_file(&stdout).unwrap();
        std::os::unix::fs::symlink(f.root().join("outside"), &stdout).unwrap();
        fs::write(f.root().join("outside"), "unsafe").unwrap();
        let status = f.run(&["status"]).unwrap();
        assert_eq!(status["tasks"][0]["observed_stdout_bytes"], Value::Null);
        assert_eq!(status["tasks"][0]["observed_stderr_bytes"], Value::Null);
        assert_eq!(
            status["tasks"][0]["observed_bytes_unavailable_reason"],
            "active attempt log files are missing or unsafe"
        );
        let shown = f.run(&["show", "task"]).unwrap();
        assert_eq!(shown["observed_stdout_bytes"], Value::Null);
        assert_eq!(shown["observed_stderr_bytes"], Value::Null);
        assert_eq!(
            shown["observed_bytes_unavailable_reason"],
            "active attempt log files are missing or unsafe"
        );
    }
    let _ = stderr;
}

pub(crate) fn status_reports_output_age_and_silence_without_exposing_log_contents() {
    let f = Fixture::new();
    f.task("active");
    f.attempt("active", "old-attempt", "session");
    let (stdout, stderr) = f.logs("old-attempt");
    fs::write(&stderr, "private-log-secret").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    let times = fs::FileTimes::new().set_modified(old);
    fs::File::options()
        .write(true)
        .open(&stdout)
        .unwrap()
        .set_times(times)
        .unwrap();
    fs::File::options()
        .write(true)
        .open(&stderr)
        .unwrap()
        .set_times(times)
        .unwrap();

    let status = f.run(&["status"]).unwrap();
    let task = &status["tasks"][0];
    assert!(task["last_output_age_seconds"].as_u64().unwrap() >= 300);
    assert_eq!(task["output_age_unavailable_reason"], Value::Null);
    assert_eq!(task["output_silence_warning"], true);
    assert_eq!(task["silence_warning_threshold_seconds"], 300);
    assert_eq!(task["latest_attempt_id"], "old-attempt");
    assert!(!status.to_string().contains("hello"));
    assert!(!status.to_string().contains("private-log-secret"));

    fs::write(stdout, "").unwrap();
    fs::write(stderr, "").unwrap();
    let status = f.run(&["status"]).unwrap();
    let task = &status["tasks"][0];
    assert!(task["last_output_age_seconds"].is_number());
    assert_eq!(task["output_silence_warning"], false);
    assert_eq!(task["output_log_status"], "no_output_yet");
    assert_eq!(task["output_age_unavailable_reason"], Value::Null);
}

pub(crate) fn status_warns_on_durable_attempt_age_with_empty_logs_without_requesting_stop() {
    let f = Fixture::new();
    f.task("old");
    f.attempt("old", "silent", "session");
    let (stdout, stderr) = f.logs("silent");
    fs::write(stdout, "").unwrap();
    fs::write(stderr, "").unwrap();
    f.db()
        .execute(
            "UPDATE attempts SET created_at=datetime('now','-600 seconds') WHERE id='silent'",
            [],
        )
        .unwrap();

    let status = f.run(&["status"]).unwrap();
    let task = &status["tasks"][0];
    assert_eq!(task["output_log_status"], "no_output_yet");
    assert!(task["last_output_age_seconds"].as_u64().unwrap() >= 300);
    assert_eq!(task["output_age_unavailable_reason"], Value::Null);
    assert_eq!(task["output_silence_warning"], true);
    assert_eq!(task["phase"], "held");
    let shown = f.run(&["show", "old"]).unwrap();
    assert_eq!(shown["phase"], "held");
    assert!(
        shown["intents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["kind"] != "stop")
    );
    assert_eq!(
        f.db()
            .query_row("SELECT state FROM tasks WHERE id='old'", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "held"
    );
    assert_eq!(
        f.db()
            .query_row(
                "SELECT lifecycle FROM attempts WHERE id='silent'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "launch_intended"
    );

    f.task("young");
    f.attempt("young", "quiet", "session");
    let (stdout, stderr) = f.logs("quiet");
    fs::write(&stdout, "").unwrap();
    fs::write(&stderr, "").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    let times = fs::FileTimes::new().set_modified(old);
    for path in [&stdout, &stderr] {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(times)
            .unwrap();
    }
    let status = f.run(&["status"]).unwrap();
    let young = status["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == "young")
        .unwrap();
    assert_eq!(young["output_log_status"], "no_output_yet");
    assert!(young["last_output_age_seconds"].as_u64().unwrap() < 300);
    assert_eq!(young["output_silence_warning"], false);
}

pub(crate) fn status_does_not_warn_without_a_valid_reserved_attempt_start() {
    let f = Fixture::new();
    f.task("task");
    f.attempt("task", "silent", "session");
    let (stdout, stderr) = f.logs("silent");
    fs::write(stdout, "").unwrap();
    fs::write(stderr, "").unwrap();
    f.db()
        .execute(
            "UPDATE attempts SET created_at='invalid' WHERE id='silent'",
            [],
        )
        .unwrap();
    let task = &f.run(&["status"]).unwrap()["tasks"][0];
    assert_eq!(task["output_silence_warning"], false);
    assert_eq!(task["last_output_age_seconds"], Value::Null);
    assert_eq!(task["output_log_status"], "unavailable");
    assert_eq!(
        task["output_age_unavailable_reason"],
        "no verified output log data or valid attempt start time"
    );

    f.db()
        .execute(
            "UPDATE attempts SET created_at=datetime('now','-600 seconds') WHERE id='silent'",
            [],
        )
        .unwrap();
    f.db()
        .execute(
            "UPDATE reservations SET status='released' WHERE attempt_id='silent'",
            [],
        )
        .unwrap();
    let task = &f.run(&["status"]).unwrap()["tasks"][0];
    assert_eq!(task["output_silence_warning"], false);
    assert_eq!(task["last_output_age_seconds"], Value::Null);
    assert_eq!(task["output_age_unavailable_reason"], "no reserved attempt");
}
