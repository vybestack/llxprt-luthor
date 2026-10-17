use super::*;
#[test]
fn native_continuation_wrong_actor_stale_revision_or_config_never_dispatches() {
    let mut lane = fixture(false);
    let original = lane.f.config.clone();
    for mutation in ["actor", "revision", "template", "session", "mapping"] {
        lane.f.config = original.clone();
        match mutation {
            "template" => lane
                .f
                .config
                .resume
                .args
                .push("--branch=conversation".into()),
            "session" => {
                let i = lane
                    .f
                    .config
                    .resume
                    .args
                    .iter()
                    .position(|a| a == "--session")
                    .unwrap();
                lane.f.config.resume.args[i + 1] = "another-session".into();
            }
            "mapping" => lane.f.config.mappings[0].base_branch = "another-base".into(),
            _ => (),
        }
        let mut launcher = Launcher {
            count: 0,
            execute: true,
        };
        assert!(
            continue_amended(
                &mut lane.f.store,
                NativeContinuationDependencies {
                    task_id: "task-a",
                    previous_attempt_id: "attempt-task-a",
                    attempt_id: "attempt-second",
                    mode: Mode::NaturalExitRetry,
                    config: &lane.f.config,
                    config_revision: if mutation == "revision" {
                        "old-revision"
                    } else {
                        "corrected-revision"
                    },
                    actor: if mutation == "actor" { "other" } else { "bot" },
                    projects: &mut lane.github,
                    prs: &mut crate::FakePr::default(),
                    launcher: &mut launcher,
                }
            )
            .is_err(),
            "{mutation}"
        );
        assert_eq!(launcher.count, 0, "{mutation}");
        assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
    }
}
#[test]
fn native_continuation_cli_rejects_changed_session_environment_before_github_reads() {
    let mut lane = fixture(false);
    let config = lane.f.dir.path().join("current-config.json");
    fs::write(&config, serde_json::to_vec(&lane.f.config).unwrap()).unwrap();
    let root = lane.f.store.root().to_path_buf();
    drop(lane.f.store);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args([
            "continue-amended",
            "--task",
            "task-a",
            "--predecessor",
            "attempt-task-a",
            "--attempt",
            "attempt-second",
            "--mode",
            "natural-exit-retry",
            "--config",
        ])
        .arg(config)
        .args([
            "--config-revision",
            "corrected-revision",
            "--actor",
            "bot",
            "--execute",
        ])
        .env(
            "XDG_CONFIG_HOME",
            lane.f.dir.path().join("changed-config-home"),
        )
        .output()
        .unwrap();
    lane.f.store = StateStore::open(&root, lane.f.config.capacity).unwrap();
    assert!(!output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(status["status"], "held");
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
    let mut launcher = Launcher {
        count: 0,
        execute: true,
    };
    let plan = run(
        &mut lane,
        Mode::NaturalExitRetry,
        "attempt-task-a",
        "attempt-second",
        "ok",
        &mut launcher,
    )
    .unwrap();
    settle(&mut lane, &plan.attempt_id);
}
