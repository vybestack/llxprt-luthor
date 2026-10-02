use std::{collections::BTreeMap, fs, path::Path, process::Command};
use tempfile::tempdir;

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

fn compile_gate(root: &Path) -> std::path::PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for directory in ["src", "tests", "xtask/src", "xtask/tests"] {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    for file in [
        "Cargo.toml",
        "Cargo.lock",
        "clippy.toml",
        "xtask/Cargo.toml",
    ] {
        fs::copy(workspace.join(file), root.join(file)).unwrap();
    }
    for file in ["tests/smoke.rs", "xtask/tests/smoke.rs"] {
        fs::write(root.join(file), "#[test] fn smoke() {}\n").unwrap();
    }
    fs::write(root.join("xtask/src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    fs::write(
        root.join("xtask/src/main.rs"),
        include_str!("../src/main.rs"),
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    for file in ["xtask/debt.json", "xtask/owners.json"] {
        fs::write(root.join(file), "[]\n").unwrap();
    }
    let libraries = dependencies(workspace);
    let deps = libraries["serde_json"].parent().unwrap();
    let binary = root.join("policy-gate");
    // Compile the unchanged entry point against the library this test is exercising.
    let output = Command::new("rustc")
        .args(["--edition=2024", "--crate-name", "local_type_gate_fixture"])
        .arg(root.join("xtask/src/main.rs"))
        .arg("--extern")
        .arg(format!("xtask={}", libraries["xtask"].display()))
        .arg("--extern")
        .arg(format!("serde_json={}", libraries["serde_json"].display()))
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
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

fn methods(count: usize) -> String {
    (0..count).map(|i| format!("fn m{i}() {{}} ")).collect()
}

#[test]
fn actual_policy_gate_accepts_twenty_and_rejects_twenty_one_local_methods() {
    let root = tempdir().unwrap();
    let gate = compile_gate(root.path());
    for (source, success, diagnostic) in [
        (
            format!("fn run() {{ struct T; impl T {{ {} }} }}", methods(20)),
            true,
            "",
        ),
        (
            format!("fn run() {{ struct T; impl T {{ {} }} }}", methods(21)),
            false,
            "T:type_methods: type_methods=21 limit=20",
        ),
        (
            format!(
                "struct T; fn run() {{ struct T; use T as Alias; impl T {{ {} }} {{ impl Alias {{ fn extra() {{}} }} }} }}",
                methods(20)
            ),
            false,
            "T:type_methods: type_methods=21 limit=20",
        ),
        (
            "fn run() { impl Missing { fn method() {} } }".into(),
            false,
            "cannot aggregate safely",
        ),
    ] {
        fs::write(root.path().join("src/lib.rs"), source).unwrap();
        let output = Command::new(&gate)
            .arg("policy")
            .current_dir(root.path())
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(i32::from(!success)), "{stderr}");
        assert!(stderr.contains(diagnostic), "{stderr}");
        if success {
            assert!(stderr.is_empty(), "{stderr}");
        }
    }
    for total in [400, 401] {
        let mut body = String::new();
        for i in 0..10 {
            let statements = 38 + usize::from(total == 401 && i == 0);
            body.push_str(&format!(
                "fn m{i}() {{\n{} }}\n",
                "let _v = 1;\n".repeat(statements)
            ));
        }
        fs::write(
            root.path().join("src/lib.rs"),
            format!("fn run() {{ struct T; impl T {{\n{body}}} }}"),
        )
        .unwrap();
        let output = Command::new(&gate)
            .arg("policy")
            .current_dir(root.path())
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        // The enclosing function also exceeds 80 lines; only 401 exceeds the type cap.
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert_eq!(stderr.contains("T:type_lines:"), total == 401, "{stderr}");
        if total == 401 {
            assert!(stderr.contains("type_lines=401 limit=400"), "{stderr}");
        }
    }
}

#[test]
fn actual_policy_gate_enforces_attribute_file_boundaries_and_suppressions() {
    let root = tempdir().unwrap();
    let gate = compile_gate(root.path());
    for lines in [799, 800, 801] {
        let mut source = String::from("#[repr(C)]\n#[derive(Debug)]\npub struct Record {\n");
        for index in 0..lines - 2 {
            source.push_str(&format!("pub field_{index}: u8,\n"));
        }
        source.push_str("}\n");
        fs::write(root.path().join("src/lib.rs"), &source).unwrap();
        let output = Command::new(&gate)
            .arg("policy")
            .current_dir(root.path())
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(
            output.status.code(),
            Some(i32::from(lines > 800)),
            "{stderr}"
        );
        if lines > 800 {
            assert!(stderr.contains("file_lines=801 limit=800"), "{stderr}");
        } else {
            assert!(stderr.is_empty(), "{stderr}");
        }
        if lines == 800 {
            fs::write(
                root.path().join("src/lib.rs"),
                format!("#[allow(warnings)]\n{source}"),
            )
            .unwrap();
            let output = Command::new(&gate)
                .arg("policy")
                .current_dir(root.path())
                .output()
                .unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(output.status.code(), Some(1), "{stderr}");
            assert!(stderr.contains("forbidden lint suppression"), "{stderr}");
            assert!(!stderr.contains("file_lines="), "{stderr}");
        }
    }
}
