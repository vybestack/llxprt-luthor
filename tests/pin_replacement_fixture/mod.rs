//! Hermetic capability-reporting builds for explicit continuation repins.
use luthor::{
    config::Config,
    state::{StateStore, journal, pin_replacement::PinReplacementRequest},
    worker_build::WorkerBuild,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

pub(crate) fn replacement(config: &mut Config, store: &mut StateStore) -> WorkerBuild {
    journal::record_evidence(
        store,
        "task",
        Some("attempt-real"),
        "session_state_format",
        &json!({"version":3}).to_string(),
    )
    .unwrap();
    let path = config
        .worktree_root
        .parent()
        .unwrap()
        .join("replacement-worker");
    build(
        config,
        &path,
        r#"printf '{"session_state_versions":[2,3]}\n'"#,
    )
}

pub(crate) fn build(config: &mut Config, path: &Path, probe: &str) -> WorkerBuild {
    fs::write(
        path,
        format!(
            r#"#!/bin/sh
if test "$#" = 1 && test "$1" = --session-state-versions; then
  {probe}
  exit 0
fi
printf '%s\n' "$*" > replacement-ran
printf '{{"session_state_version":3}}\n'
exit 2
"#
        ),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    config.resume.executable = path.into();
    WorkerBuild::capture(path, Path::new("/")).unwrap()
}

pub(crate) fn request(build: &WorkerBuild) -> PinReplacementRequest<'_> {
    PinReplacementRequest {
        executable: &build.path,
        sha256: &build.sha256,
        actor: "operator",
        reason: "provider protocol fix; worker attests unchanged session format",
    }
}

pub(crate) fn view(config: &Config) -> Value {
    serde_json::from_str(
        &luthor::cli::execute(&config.state_root, &["show".into(), "task".into()]).unwrap(),
    )
    .unwrap()
}

pub(crate) fn cli(args: &[String], failure: bool) -> Value {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        !output.status.success(),
        failure,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

pub(crate) fn args(config: &Config, build: &WorkerBuild) -> Vec<String> {
    let path = config
        .worktree_root
        .parent()
        .unwrap()
        .join("repin-config.json");
    fs::write(&path, serde_json::to_vec(config).unwrap()).unwrap();
    vec![
        "repin".into(),
        "task".into(),
        "--config".into(),
        path.to_str().unwrap().into(),
        "--executable".into(),
        build.path.to_str().unwrap().into(),
        "--sha256".into(),
        build.sha256.clone(),
        "--actor".into(),
        "operator".into(),
        "--reason".into(),
        request(build).reason.into(),
    ]
}

pub(crate) fn db(config: &Config) -> rusqlite::Connection {
    rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap()
}
