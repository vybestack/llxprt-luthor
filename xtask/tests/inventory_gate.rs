use std::{collections::BTreeMap, fs, path::Path, process::Command};

fn dependencies(workspace: &Path) -> BTreeMap<String, std::path::PathBuf> {
    let output = Command::new("cargo")
        .args([
            "build",
            "--lib",
            "--offline",
            "--locked",
            "--message-format=json",
            "--manifest-path",
        ])
        .arg(workspace.join("xtask/Cargo.toml"))
        .current_dir(workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut libraries = BTreeMap::new();
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        if event["reason"] != "compiler-artifact" {
            continue;
        }
        let name = event["target"]["name"].as_str().unwrap();
        if !["xtask", "serde_json"].contains(&name) {
            continue;
        }
        for filename in event["filenames"].as_array().unwrap() {
            let path = std::path::PathBuf::from(filename.as_str().unwrap());
            if path.extension().is_some_and(|e| e == "rlib") {
                assert!(
                    libraries.insert(name.into(), path).is_none(),
                    "duplicate artifact for {name}"
                );
            }
        }
    }
    assert_eq!(
        libraries.len(),
        2,
        "missing compiler artifacts: {libraries:?}"
    );
    libraries
}

fn cargo(root: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new("cargo")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn fixture(root: &Path) {
    for directory in ["src", "tests", "xtask/src", "xtask/tests"] {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"luthor\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\nmembers = [\"xtask\"]\nresolver = \"3\"\n").unwrap();
    fs::write(
        root.join("xtask/Cargo.toml"),
        "[package]\nname = \"xtask\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    for file in ["src/lib.rs", "xtask/src/lib.rs"] {
        fs::write(root.join(file), "pub fn fixture() {}\n").unwrap();
    }
    fs::write(root.join("xtask/src/main.rs"), "fn main() {}\n").unwrap();
    for file in ["tests/smoke.rs", "xtask/tests/smoke.rs"] {
        fs::write(root.join(file), "#[test] fn smoke() {}\n").unwrap();
    }
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("clippy.toml"),
        root.join("clippy.toml"),
    )
    .unwrap();
    cargo(root, &["generate-lockfile", "--offline"]);
}

fn compile_gate(root: &Path) -> std::path::PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let libraries = dependencies(workspace);
    let source = root.join("gate-main.txt");
    fs::write(&source, include_str!("../src/main.rs")).unwrap();
    let binary = root.join("policy-gate");
    let output = Command::new("rustc")
        .args(["--edition=2024", "--crate-name", "inventory_gate_fixture"])
        .arg(&source)
        .arg("--extern")
        .arg(format!("xtask={}", libraries["xtask"].display()))
        .arg("--extern")
        .arg(format!("serde_json={}", libraries["serde_json"].display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            libraries["serde_json"].parent().unwrap().display()
        ))
        .arg("-o")
        .arg(&binary)
        .env("CARGO_MANIFEST_DIR", root.join("xtask"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

fn policy(gate: &Path, root: &Path) -> std::process::Output {
    Command::new(gate)
        .arg("policy")
        .current_dir(root)
        .output()
        .unwrap()
}

fn adversarial_target(root: &Path, package: &str, kind: &str) -> std::path::PathBuf {
    let prefix = if kind == "test" { "tests" } else { "src" };
    let source = format!("{prefix}/hidden.txt");
    let package_root = root.join(package);
    let path = package_root.join(&source);
    let body = format!(
        "#[allow(dead_code)]\nfn main() {{\n{}}}\n",
        "let _value = 1;\n".repeat(81)
    );
    assert!(
        !xtask::suppression::check("hidden.txt", &body)
            .unwrap()
            .is_empty()
    );
    assert!(
        xtask::metrics::analyze("hidden.txt", &body)
            .unwrap()
            .functions
            .iter()
            .any(|f| f.lines > 80)
    );
    fs::write(&path, body).unwrap();
    let manifest_path = package_root.join("Cargo.toml");
    let mut manifest = fs::read_to_string(&manifest_path).unwrap();
    if kind == "lib" {
        manifest.push_str(&format!("\n[lib]\npath = {source:?}\n"));
    } else {
        manifest.push_str(&format!(
            "\n[[{kind}]]\nname = \"hidden\"\npath = {source:?}\n"
        ));
    }
    fs::write(manifest_path, manifest).unwrap();
    path
}

#[test]
fn executable_policy_rejects_cargo_compilable_non_rs_lib_bin_and_test_targets() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fixture(&root);
    let gate = compile_gate(&root);
    let output = policy(&gate, &root);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    for package in ["", "xtask"] {
        for kind in ["lib", "bin", "test"] {
            fixture(&root);
            let path = adversarial_target(&root, package, kind);
            let output = cargo(
                &root,
                &[
                    "metadata",
                    "--offline",
                    "--locked",
                    "--no-deps",
                    "--format-version",
                    "1",
                ],
            );
            let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(metadata["packages"].as_array().unwrap().iter().any(|p| {
                p["targets"].as_array().unwrap().iter().any(|t| {
                    t["src_path"].as_str() == path.to_str()
                        && t["kind"] == serde_json::json!([kind])
                })
            }));
            cargo(
                &root,
                &[
                    "check",
                    "--workspace",
                    "--all-targets",
                    "--offline",
                    "--locked",
                ],
            );
            let scan =
                xtask::scan::scan(&root, &["src", "tests", "xtask/src", "xtask/tests"]).unwrap();
            assert!(scan.suppressions.is_empty());
            let output = policy(&gate, &root);
            assert_eq!(output.status.code(), Some(1));
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(
                stderr.contains("gate: unsupported compiled target source extension"),
                "{package}/{kind}: {stderr}"
            );
            assert!(stderr.contains(path.to_str().unwrap()), "{stderr}");
            fs::remove_file(path).unwrap();
        }
    }
}
