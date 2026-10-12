use std::{fs, path::Path, process::Command};

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A real, isolated remote served over a fixture SSH transport. Keep the mapped
/// GitHub URL intact so production repository/remote identity checks still run.
/// This transport cannot contact GitHub or use the operator's SSH configuration.
pub fn remote(checkout: &Path) {
    let remote = checkout.parent().unwrap().join("remote.git");
    git(
        checkout,
        &[
            "clone",
            "--bare",
            checkout.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    let ssh = remote.join("fixture-ssh");
    fs::write(
        &ssh,
        format!("#!/bin/sh\nexec git-upload-pack '{}'\n", remote.display()),
    )
    .unwrap();
    git(
        checkout,
        &[
            "config",
            "core.sshCommand",
            &format!("sh '{}'", ssh.display()),
        ],
    );
    git(checkout, &["config", "ssh.variant", "ssh"]);
}
