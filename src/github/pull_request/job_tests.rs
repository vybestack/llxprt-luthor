use super::job_reader::{logs, rerun};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

fn gh(script: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gh");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    (dir, path)
}

#[test]
fn failed_job_rerun_verifies_authenticated_identity_and_exact_head_before_post() {
    let (dir, executable) =
        gh("#!/bin/sh\ntest -f \"$0.intent\" || exit 1\nprintf '%s\\n' \"$*\" > \"$0.called\"\n");
    for scenario in [
        "identity",
        "id",
        "head",
        "repository",
        "status",
        "missing_status",
        "valid",
    ] {
        let mut calls = 0;
        let result = rerun(
            &executable,
            "org/code",
            12,
            "head",
            |_| {
                calls += 1;
                if calls == 1 {
                    return Ok(
                        json!({"login":if scenario == "identity" {"other"} else {"acoliver"}}),
                    );
                }
                Ok(json!({
                    "id":if scenario == "id" {13} else {12},
                    "head_sha":if scenario == "head" {"other"} else {"head"},
                    "repository":{"full_name":if scenario == "repository" {"other/code"} else {"org/code"}},
                    "status":match scenario { "status" => json!("unknown"), "missing_status" => json!(null), _ => json!("completed") }
                }))
            },
            || {
                fs::write(executable.with_extension("intent"), "intent").unwrap();
                Ok::<(), super::LookupError>(())
            },
        );
        match scenario {
            "valid" => assert_eq!(result.unwrap(), super::FailedJobsRerun::Accepted),
            "identity" => assert_eq!(
                result.unwrap_err().category,
                super::ErrorCategory::Permission
            ),
            _ => assert_eq!(
                result.unwrap_err().category,
                super::ErrorCategory::Malformed
            ),
        }
        assert_eq!(
            executable.with_extension("intent").exists(),
            scenario == "valid"
        );
        assert_eq!(dir.path().join("gh.called").exists(), scenario == "valid");
    }
    let posted = fs::read_to_string(dir.path().join("gh.called")).unwrap();
    assert_eq!(
        posted.trim(),
        "api --method POST repos/org/code/actions/runs/12/rerun-failed-jobs"
    );
}

#[test]
fn unfinished_failed_runs_are_pending_without_intent_or_post() {
    let (dir, executable) = gh("#!/bin/sh\necho called > \"$0.called\"\n");
    for status in ["in_progress", "queued"] {
        let mut calls = 0;
        let result = rerun(
            &executable,
            "org/code",
            12,
            "head",
            |_| {
                calls += 1;
                Ok(if calls == 1 {
                    json!({"login":"acoliver"})
                } else {
                    json!({"id":12,"head_sha":"head","repository":{"full_name":"org/code"},"status":status})
                })
            },
            || -> Result<(), super::LookupError> { panic!("pending run must not record intent") },
        );
        assert_eq!(result.unwrap(), super::FailedJobsRerun::Pending);
        assert_eq!(calls, 2);
        assert!(!dir.path().join("gh.called").exists());
    }
}

#[test]
fn failed_rerun_intent_prevents_post_and_rejected_post_is_not_accepted() {
    let (dir, executable) = gh("#!/bin/sh\necho called > \"$0.called\"\nexit 1\n");
    for intent_error in [true, false] {
        let mut calls = 0;
        let result = rerun(
            &executable,
            "org/code",
            12,
            "head",
            |_| {
                calls += 1;
                Ok(if calls == 1 {
                    json!({"login":"acoliver"})
                } else {
                    json!({"id":12,"head_sha":"head","repository":{"full_name":"org/code"},"status":"completed"})
                })
            },
            || {
                if intent_error {
                    Err(super::types::error(
                        super::ErrorCategory::Unknown,
                        "intent-failed",
                        None,
                    ))
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            result.unwrap_err().code,
            if intent_error {
                "intent-failed"
            } else {
                "rerun-command-failed"
            }
        );
        assert_eq!(dir.path().join("gh.called").exists(), !intent_error);
    }
}

#[test]
fn failed_job_logs_are_bounded_and_command_failure_is_not_a_successful_excerpt() {
    let (_dir, executable) = gh(
        "#!/bin/sh\ni=0; while test $i -lt 1000; do printf 'diagnostic line %s\\n' \"$i\"; i=$((i+1)); done\n",
    );
    let excerpt = logs(&executable, "org/code", 34).unwrap();
    assert!(excerpt.len() <= 4096);
    assert!(excerpt.contains("diagnostic line 999"));
    fs::write(&executable, "#!/bin/sh\necho partial-log\nexit 1\n").unwrap();
    assert!(logs(&executable, "org/code", 34).is_err());
}
