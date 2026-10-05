use super::AmendmentFixture;
use std::{fs, os::unix::fs::PermissionsExt};

fn invalid_vectors(good: &[String]) -> Vec<Vec<String>> {
    let mut bad = vec![vec![], good[..12].to_vec()];
    for index in [0, 2, 4, 6, 8, 10] {
        let mut missing = good.to_vec();
        missing.drain(index..index + 2);
        bad.push(missing);
        let mut duplicate = good.to_vec();
        duplicate.extend_from_slice(&good[index..index + 2]);
        bad.push(duplicate);
        for invalid in ["", "   ", "--execute", "--unknown"] {
            let mut empty = good.to_vec();
            empty[index + 1] = invalid.into();
            bad.push(empty);
        }
        let mut unknown = good.to_vec();
        unknown[index] = "--unknown".into();
        bad.push(unknown);
    }
    for trailing in ["--execute", "unexpected"] {
        let mut args = good.to_vec();
        args.push(trailing.into());
        bad.push(args);
    }
    let mut reason = good.to_vec();
    reason[11] = "something_else".into();
    bad.push(reason);
    let mut positional = good.to_vec();
    positional[0] = good[1].clone();
    bad.push(positional);
    assert_eq!(bad.len(), 48);
    bad
}

#[test]
fn amendment_cli_bad_arguments_never_read_config_or_open_writable_state() {
    let case = AmendmentFixture::new();
    let f = &case.f;
    let tables = [
        "tasks",
        "attempts",
        "reservations",
        "intents",
        "evidence",
        "state_meta",
    ];
    let before = f.rows(&tables);
    fs::set_permissions(&f.config.state_root, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&f.config_path, f.dir.path().join("hidden-config.json")).unwrap();
    let good = case.args();
    let mut bad = invalid_vectors(&good);
    for (index, value) in [
        (1, "../task"),
        (3, "a\nforged"),
        (9, "actor login"),
        (7, "revision\nleak"),
        (7, "revision\rleak"),
    ] {
        let mut args = good.clone();
        args[index] = value.into();
        bad.push(args);
    }
    for index in [1, 3, 7, 9] {
        let mut args = good.clone();
        args[index] = "x".repeat(129);
        bad.push(args);
    }
    for args in bad {
        let out = case.run(&args);
        assert!(!out.status.success(), "accepted {args:?}");
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).starts_with("luthor: expected --task TASK"));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("leak"));
    }
    assert_eq!(f.rows(&tables), before);
    assert_eq!(
        fs::metadata(&f.config.state_root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert!(fs::read(&f.calls).unwrap().is_empty());
    assert!(!f.marker.exists());
}

#[test]
fn amendment_cli_configuration_errors_are_bounded_before_state_access() {
    let case = AmendmentFixture::new();
    for content in ["not json private-data", "{\"private-data\":true}"] {
        fs::write(&case.f.config_path, content).unwrap();
        let before = case
            .f
            .rows(&["tasks", "attempts", "reservations", "intents", "evidence"]);
        let out = case.run(&case.args());
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "luthor: amendment configuration invalid\n"
        );
        assert_eq!(
            before,
            case.f
                .rows(&["tasks", "attempts", "reservations", "intents", "evidence"])
        );
        assert!(fs::read(&case.f.calls).unwrap().is_empty());
    }
}

#[test]
fn amendment_cli_help_lists_separate_explicit_consent_command() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_luthor"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = String::from_utf8(out.stdout).unwrap();
    assert!(help.contains("amend-undispatched --task TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason native_initial_branch_is_conversation --execute"));
    assert!(help.contains("continue-undispatched"));
}
