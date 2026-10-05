use super::AmendmentFixture;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Output, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn amendment_cli_requires_exact_corrected_snapshot_without_altering_original() {
    for mutation in ["uncorrected", "resume", "initial", "mapping"] {
        let case = AmendmentFixture::new();
        let mut config = case.corrected.clone();
        match mutation {
            "uncorrected" => config = case.f.config.clone(),
            "resume" => config.resume.args.push("changed".into()),
            "initial" => config.initial.args.push("changed".into()),
            "mapping" => config.mappings[0].base_branch = "other".into(),
            _ => unreachable!(),
        }
        fs::write(&case.f.config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        let before = case
            .f
            .rows(&["tasks", "attempts", "reservations", "intents"]);
        let bytes = fs::read(&case.f.config_path).unwrap();
        case.held("config_changed");
        assert_eq!(
            case.f
                .rows(&["tasks", "attempts", "reservations", "intents"]),
            before
        );
        assert_eq!(fs::read(&case.f.config_path).unwrap(), bytes);
        assert!(fs::read(&case.f.calls).unwrap().is_empty());
        assert_eq!(case.f.count("evidence", "initial_branch_removed"), 0);
        case.assert_reserved_original();
    }
}

#[test]
fn amendment_cli_nonprivate_attempt_namespace_never_repairs_it() {
    let case = AmendmentFixture::new();
    let namespace = case.f.config.state_root.join("attempts");
    fs::set_permissions(&namespace, fs::Permissions::from_mode(0o755)).unwrap();
    case.held("storage_unavailable");
    assert_eq!(
        fs::metadata(&namespace).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(case.f.count("evidence", "initial_branch_removed"), 0);
    assert_eq!(case.f.count("intents", "supervisor_dispatch"), 0);
    assert!(!case.f.marker.exists());
    case.assert_reserved_original();
}

fn block_second_observation(case: &AmendmentFixture) {
    let root = case.f.dir.path();
    let gh = root.join("gh");
    let original = fs::read_to_string(&gh).unwrap();
    let replacement = format!(
        r#" graphql)
if awk '/^api graphql/ {{ n++ }} END {{ exit n < 2 }}' '{root}/gh-calls'; then
  touch '{root}/post-observing'
  while ! test -f '{root}/post-release'; do sleep 0.02; done
fi
cat"#,
        root = root.display()
    );
    assert!(original.contains(" graphql) cat"));
    fs::write(gh, original.replace(" graphql) cat", &replacement)).unwrap();
}

fn post_authorization_run(
    case: &AmendmentFixture,
    mutate: impl FnOnce(&AmendmentFixture),
) -> Output {
    block_second_observation(case);
    let mut child = case
        .command(&case.args())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !case.f.dir.path().join("post-observing").exists() {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(case.f.count("evidence", "initial_branch_removed"), 1);
    assert!(luthor::state::StateStore::open(&case.f.config.state_root, 1).is_err());
    mutate(case);
    fs::write(case.f.dir.path().join("post-release"), "").unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn amendment_cli_post_authorization_namespace_drift_keeps_audit_and_refuses_replay() {
    let (case, out) = super::fresh_os_run(|case| {
        post_authorization_run(case, |case| {
            let namespace = case.f.config.state_root.join("attempts");
            fs::set_permissions(namespace, fs::Permissions::from_mode(0o755)).unwrap();
        })
    });
    assert!(!out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["reason"], "storage_unavailable");
    assert_eq!(case.f.count("intents", "supervisor_dispatch"), 0);
    assert!(!case.f.marker.exists());
    assert_eq!(
        fs::read_dir(case.f.config.state_root.join("attempts"))
            .unwrap()
            .count(),
        0
    );
    super::success::assert_replay(&case);
}

#[test]
fn amendment_cli_uncertain_committed_dispatch_keeps_reservation_and_cannot_relaunch() {
    let case = AmendmentFixture::new();
    case.f.db().execute_batch("CREATE TRIGGER dispatch_drift AFTER INSERT ON intents WHEN NEW.kind='supervisor_dispatch' BEGIN INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(NEW.task_id,NEW.attempt_id,'unknown-injected','private-worker-data'); END;").unwrap();
    case.held("launch_failed");
    assert_eq!(case.f.count("intents", "supervisor_dispatch"), 1);
    assert_eq!(case.f.count("evidence", "initial_branch_removed"), 1);
    assert!(!case.f.marker.exists());
    assert!(
        !case
            .f
            .config
            .state_root
            .join(format!("attempts/{}.receipt.json", case.f.attempt))
            .exists()
    );
    super::success::assert_replay(&case);
}

#[test]
fn amendment_cli_wrong_actor_or_uncertain_remote_refuses_before_authorization() {
    for failure in ["actor", "identity", "pr", "source"] {
        let case = AmendmentFixture::new();
        let root = case.f.dir.path();
        let mut args = case.args();
        let reason = match failure {
            "actor" => {
                args[9] = "other".into();
                "actor_mismatch"
            }
            "identity" => {
                let gh = root.join("gh");
                let original = fs::read_to_string(&gh).unwrap();
                fs::write(
                    gh,
                    original.replace("printf 'acoliver\\n'", "printf 'other\\n'"),
                )
                .unwrap();
                "identity_unavailable"
            }
            "pr" => {
                fs::write(root.join("prs.json"), "{\"message\":\"private-error\"}").unwrap();
                "pr_unavailable"
            }
            "source" => {
                fs::write(root.join("issue.json"), "private-error").unwrap();
                "source_unavailable"
            }
            _ => unreachable!(),
        };
        let out = case.run(&args);
        assert!(!out.status.success());
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["reason"], reason);
        assert!(!String::from_utf8_lossy(&out.stdout).contains("private-error"));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("private-error"));
        assert_eq!(case.f.count("evidence", "initial_branch_removed"), 0);
        assert_eq!(case.f.count("intents", "supervisor_dispatch"), 0);
        case.assert_reserved_original();
    }
}

#[test]
fn amendment_cli_capacity_mismatch_refuses_state_open_without_remote_reads() {
    let case = AmendmentFixture::new();
    let mut config = case.corrected.clone();
    config.capacity = 2;
    fs::write(&case.f.config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let before = case.f.rows(&[
        "tasks",
        "attempts",
        "reservations",
        "intents",
        "evidence",
        "state_meta",
    ]);
    let out = case.run(&case.args());
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "luthor: amendment state unavailable\n"
    );
    assert!(fs::read(&case.f.calls).unwrap().is_empty());
    assert_eq!(
        case.f.rows(&[
            "tasks",
            "attempts",
            "reservations",
            "intents",
            "evidence",
            "state_meta"
        ]),
        before
    );
    case.assert_reserved_original();
}

#[test]
fn amendment_cli_current_revision_drift_after_audit_cannot_dispatch() {
    let (case, out) = super::fresh_os_run(|case| {
        post_authorization_run(case, |case| {
            case.f.db().execute_batch("UPDATE evidence SET payload=json_set(payload,'$.current_config_revision','wrong-revision') WHERE kind='initial_branch_removed'").unwrap();
        })
    });
    assert!(!out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["reason"], "authorization_failed");
    assert_eq!(case.f.count("intents", "supervisor_dispatch"), 0);
    assert!(!case.f.marker.exists());
    super::success::assert_replay(&case);
}

#[test]
fn amendment_cli_database_error_has_structured_bounded_attempt_result() {
    let case = AmendmentFixture::new();
    fs::write(case.f.dir.path().join("issue.json"), "private-remote-data").unwrap();
    case.f.db().execute_batch("CREATE TRIGGER held_failure BEFORE INSERT ON evidence WHEN NEW.kind='held_reason' BEGIN SELECT RAISE(ABORT,'private-db-data'); END;").unwrap();
    let before = case
        .f
        .rows(&["tasks", "attempts", "reservations", "intents", "evidence"]);
    let out = case.run(&case.args());
    assert!(!out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"command":"amend-undispatched", "task_id":case.f.task,
        "attempt_id":case.f.attempt,"status":"failed","reason":"state_unavailable"})
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "luthor: amendment could not be completed\n"
    );
    assert_eq!(
        case.f
            .rows(&["tasks", "attempts", "reservations", "intents", "evidence"]),
        before
    );
    case.assert_reserved_original();
}
