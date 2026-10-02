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
        "[package]\nname = \"xtask\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nquote = \"1\"\n",
    )
    .unwrap();
    for file in ["src/lib.rs", "xtask/src/lib.rs"] {
        fs::write(root.join(file), "pub fn fixture() {}\n").unwrap();
    }
    fs::write(root.join("xtask/src/main.rs"), "fn main() {}\n").unwrap();
    for file in ["tests/smoke.rs", "xtask/tests/smoke.rs"] {
        fs::write(root.join(file), "#[test] fn smoke() {}\n").unwrap();
    }
    for file in ["xtask/debt.json", "xtask/owners.json"] {
        fs::write(root.join(file), "[]\n").unwrap();
    }
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("clippy.toml"),
        root.join("clippy.toml"),
    )
    .unwrap();
    let home = root.join("tmp/cargo-home");
    fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(
        std::env::var_os("CARGO_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cargo")
            })
            .join("registry"),
        home.join("registry"),
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
        .args(["--edition=2024", "--crate-name", "quote_identity_fixture"])
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
        .env("CARGO_HOME", root.join("tmp/cargo-home"))
        .current_dir(root)
        .output()
        .unwrap()
}

fn executable(invocation: &str, suppression: bool) -> String {
    let attribute = if suppression {
        "#[allow(warnings)]"
    } else {
        ""
    };
    let operations = "operation()?; ".repeat(26);
    format!(
        "use std::format as quote;
        pub fn operation() -> Result<(), ()> {{ Ok(()) }}
        pub fn run() -> Result<String, ()> {{
            let value = {invocation}(\"{{}}\", {{
                {attribute} fn hidden() {{}}
                hidden(); {operations} 1
            }});
            Ok(value)
        }}"
    )
}

fn rejected(source: &str) {
    assert!(xtask::metrics::analyze("xtask/src/lib.rs", source).is_err());
    assert!(xtask::suppression::check("xtask/src/lib.rs", source).is_err());
    assert!(xtask::coupling::edges("xtask/src/lib.rs", source, &Default::default()).is_err());
}

#[test]
fn executable_quote_aliases_fail_closed_at_every_macro_depth() {
    for source in [
        executable("quote!", true),
        format!(
            "fn run() {{ let _ = vec![{{ {} }}]; }}",
            executable("quote!", true)
        ),
        "mod quote { pub use std::format as quote; }
         fn run() { let _ = quote::quote!(\"{}\", { #[allow(warnings)] fn hidden() {} 1 }); }"
            .into(),
        "extern crate std as quote;
         fn run() { let _ = ::quote::format!(\"{}\", 1); }"
            .into(),
        "extern crate std as quote;
         fn run() { let _ = ::quote::quote!(fn hidden() {}); }"
            .into(),
        "fn run() { let _ = vec![{ extern crate std as quote; 1 }]; }".into(),
        "use quote::quote; fn run() { let _ = quote!(fn example() {}); }".into(),
        "pub use std::format as quote; fn run() { let _ = quote!(\"{}\", 1); }".into(),
    ] {
        rejected(&source);
    }
}

fn compiled_policy(root: &Path, gate: &Path, source: &str, accepted: bool) {
    fs::write(root.join("xtask/src/lib.rs"), source).unwrap();
    cargo(
        root,
        &[
            "check",
            "--workspace",
            "--all-targets",
            "--offline",
            "--locked",
        ],
    );
    let scan = xtask::scan::scan(root, &["src", "tests", "xtask/src", "xtask/tests"]);
    assert_eq!(scan.is_ok(), accepted, "{scan:?}");
    let output = policy(gate, root);
    assert_eq!(
        output.status.code(),
        Some(if accepted { 0 } else { 1 }),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!(
        "compiled fixture: cargo check=0, scan accepted={accepted}, policy={}",
        output.status.code().unwrap()
    );
    if !accepted {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("macro"));
        eprintln!("{stderr}");
    }
}

#[test]
fn compiled_renamed_format_cannot_hide_complexity_or_suppression_from_policy() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fixture(&root);
    let gate = compile_gate(&root);
    for suppression in [false, true] {
        let renamed = executable("quote!", suppression);
        let visible = renamed
            .replace("use std::format as quote;", "")
            .replace("quote!", "std::format!");
        let report = xtask::metrics::analyze(
            "xtask/src/lib.rs",
            &visible.replace("#[allow(warnings)]", ""),
        )
        .unwrap();
        if suppression {
            assert!(
                xtask::metrics::analyze("xtask/src/lib.rs", &visible)
                    .unwrap_err()
                    .contains("forbidden lint suppression")
            );
        }
        assert!(
            report
                .functions
                .iter()
                .any(|f| f.symbol == "root::run" && f.cyclomatic == 27)
        );
        assert_eq!(
            xtask::suppression::check("xtask/src/lib.rs", &visible)
                .unwrap()
                .is_empty(),
            !suppression
        );
        compiled_policy(&root, &gate, &renamed, false);
        let nested = renamed
            .replace("let value = quote!", "let value = vec![quote!")
            .replace("1\n            });", "1\n            })].remove(0);");
        assert_ne!(nested, renamed);
        compiled_policy(&root, &gate, &nested, false);
    }
    compiled_policy(
        &root,
        &gate,
        "mod quote { pub fn shadow() {} }
        pub fn tokens() -> String {
            quote::shadow();
            vec![::quote::quote!(fn example() { #[allow(warnings)] custom!(unknown!()); })]
                .remove(0).to_string()
        }",
        true,
    );
    compiled_policy(&root, &gate,
        "mod quote { pub use std::format as quote; }
         pub fn run() -> String { quote::quote!(\"{}\", { #[allow(warnings)] fn hidden() {} hidden(); 1 }) }",
        false);
}

#[test]
fn compiled_absolute_quote_rejects_a_replacement_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    fixture(&root);
    let gate = compile_gate(&root);
    let replacement = dir.path().join("replacement");
    fs::create_dir_all(replacement.join("src")).unwrap();
    fs::write(
        replacement.join("Cargo.toml"),
        "[package]\nname = \"quote\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        replacement.join("src/lib.rs"),
        "pub use std::format as quote;",
    )
    .unwrap();
    let manifest = root.join("xtask/Cargo.toml");
    let original = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &manifest,
        format!("{original}imposter = {{ package = \"quote\", path = \"../../replacement\" }}\n"),
    )
    .unwrap();
    cargo(&root, &["generate-lockfile", "--offline"]);
    let source = executable("::quote::quote!", true).replace("use std::format as quote;", "");
    compiled_policy(
        &root,
        &gate,
        &format!("extern crate imposter as quote;\n{source}"),
        false,
    );
    compiled_policy(
        &root,
        &gate,
        &source
            .replace("let value =", "extern crate imposter as quote; let value =")
            .replace("::quote::quote!", "quote::quote!"),
        false,
    );
    fs::write(
        &manifest,
        original.replace("quote = \"1\"", "quote = { path = \"../../replacement\" }"),
    )
    .unwrap();
    cargo(&root, &["generate-lockfile", "--offline"]);
    let source = executable("::quote::quote!", true).replace("use std::format as quote;", "");
    cargo_source(&root, &source);
    let error =
        xtask::scan::scan(&root, &["src", "tests", "xtask/src", "xtask/tests"]).unwrap_err();
    assert!(error.contains("quote macro identity"), "{error}");
    let output = policy(&gate, &root);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("quote macro identity"));
}

fn cargo_source(root: &Path, source: &str) {
    fs::write(root.join("xtask/src/lib.rs"), source).unwrap();
    cargo(
        root,
        &[
            "check",
            "--workspace",
            "--all-targets",
            "--offline",
            "--locked",
        ],
    );
}
