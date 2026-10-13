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
    let (dir, executable) = gh("#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$0.called\"\n");
    for scenario in ["identity", "head", "repository", "status", "valid"] {
        let mut calls = 0;
        let result = rerun(&executable, "org/code", 12, "head", |_| {
            calls += 1;
            if calls == 1 {
                return Ok(json!({"login":if scenario == "identity" {"other"} else {"acoliver"}}));
            }
            Ok(
                json!({"id":12,"head_sha":if scenario=="head" {"other"} else {"head"}, "repository":{"full_name":if scenario=="repository" {"other/code"} else {"org/code"}},"status":if scenario=="status" {"in_progress"} else {"completed"}}),
            )
        });
        assert_eq!(result.is_ok(), scenario == "valid");
        assert_eq!(dir.path().join("gh.called").exists(), scenario == "valid");
    }
    let posted = fs::read_to_string(dir.path().join("gh.called")).unwrap();
    assert_eq!(
        posted.trim(),
        "api --method POST repos/org/code/actions/runs/12/rerun-failed-jobs"
    );
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
