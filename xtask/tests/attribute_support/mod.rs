use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use xtask::{ledger, measurements, metrics, scan};

pub struct Fixture {
    _directory: tempfile::TempDir,
    pub root: PathBuf,
    gate: PathBuf,
}

fn cargo(command: &mut Command, arguments: &[&str]) -> std::process::Output {
    let output = command.args(arguments).output().unwrap();
    assert!(
        output.status.success(),
        "{arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn libraries() -> BTreeMap<String, PathBuf> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let output = cargo(
        Command::new("cargo").current_dir(workspace),
        &[
            "build",
            "--lib",
            "--offline",
            "--locked",
            "--message-format=json",
            "-p",
            "xtask",
        ],
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
            let path = PathBuf::from(filename.as_str().unwrap());
            if path.extension().is_some_and(|e| e == "rlib") {
                assert!(libraries.insert(name.into(), path).is_none());
            }
        }
    }
    assert_eq!(libraries.len(), 2);
    libraries
}

fn compile_gate(root: &Path) -> PathBuf {
    let libraries = libraries();
    let source = root.join("gate.txt");
    fs::write(&source, include_str!("../../src/main.rs")).unwrap();
    let gate = root.join("policy-gate");
    let output = Command::new("rustc")
        .args(["--edition=2024", "--crate-name", "attribute_fixture_gate"])
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
        .arg(&gate)
        .env("CARGO_MANIFEST_DIR", root.join("xtask"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    gate
}

impl Fixture {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        for dir in ["src", "tests", "xtask/src", "xtask/tests", "tmp/macro/src"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        fs::copy(workspace.join("clippy.toml"), root.join("clippy.toml")).unwrap();
        for file in ["xtask/debt.json", "xtask/owners.json"] {
            fs::write(root.join(file), "[]\n").unwrap();
        }
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"luthor\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\nmembers = [\"xtask\"]\nexclude = [\"tmp/macro\", \"tmp/serde\", \"tmp/thiserror\", \"tmp/core\", \"tmp/serde_derive\"]\nresolver = \"3\"\n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\nthiserror = \"2\"\nfixture_macro = { path = \"tmp/macro\" }\n").unwrap();
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
        fs::write(root.join("tmp/macro/Cargo.toml"), "[package]\nname = \"fixture_macro\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n[lib]\nproc-macro = true\n").unwrap();
        fs::write(root.join("tmp/macro/src/lib.rs"), macro_source()).unwrap();
        let gate = compile_gate(&root);
        let result = Self {
            _directory: directory,
            root,
            gate,
        };
        result.lock();
        result
    }

    pub fn cargo(&self) -> Command {
        let mut command = Command::new("cargo");
        // These workspaces reuse package identities but compile different sources.
        command
            .current_dir(&self.root)
            .env("CARGO_TARGET_DIR", self.root.join("tmp/target"));
        command
    }

    pub fn lock(&self) {
        cargo(&mut self.cargo(), &["generate-lockfile", "--offline"]);
    }

    pub fn manifest(&self, from: &str, to: &str) {
        let path = self.root.join("Cargo.toml");
        let source = fs::read_to_string(&path).unwrap();
        assert!(source.contains(from));
        fs::write(path, source.replace(from, to)).unwrap();
        self.lock();
    }

    pub fn suppressed(&self, label: &str, source: &str) {
        fs::write(self.root.join("src/lib.rs"), source).unwrap();
        cargo(
            &mut self.cargo(),
            &[
                "check",
                "--workspace",
                "--all-targets",
                "--offline",
                "--locked",
            ],
        );
        let scan = scan::scan(&self.root, &["src", "tests", "xtask/src", "xtask/tests"]).unwrap();
        assert!(
            scan.suppressions
                .iter()
                .any(|s| s.contains("forbidden lint suppression")),
            "{label}"
        );
        assert!(
            ledger::validate(
                &measurements::collect(&scan, metrics::Limits::default()),
                &[]
            )
            .is_empty()
        );
        let output = Command::new(&self.gate)
            .arg("policy")
            .current_dir(&self.root)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{label}: {stderr}");
        assert!(
            stderr.contains("forbidden lint suppression"),
            "{label}: {stderr}"
        );
        eprintln!("{label}: compile=0, scan suppression found, actual policy=1 {stderr}");
    }

    pub fn compiled(&self, label: &str, source: &str, accepted: bool, diagnostic: &str) {
        fs::write(self.root.join("src/lib.rs"), source).unwrap();
        cargo(
            &mut self.cargo(),
            &[
                "check",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--offline",
                "--locked",
            ],
        );
        let scan = scan::scan(&self.root, &["src", "tests", "xtask/src", "xtask/tests"]);
        assert_eq!(scan.is_ok(), accepted, "{label}: {scan:?}");
        if accepted {
            let scan = scan.unwrap();
            assert!(scan.suppressions.is_empty());
            let measured = measurements::collect(&scan, metrics::Limits::default());
            assert!(ledger::validate(&measured, &[]).is_empty(), "{label}");
            eprintln!(
                "{label}: file_lines={}",
                scan.reports
                    .iter()
                    .find(|r| r.path == "src/lib.rs")
                    .unwrap()
                    .file_lines
            );
        } else {
            assert!(scan.unwrap_err().contains(diagnostic), "{label}");
        }
        let output = Command::new(&self.gate)
            .arg("policy")
            .current_dir(&self.root)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(i32::from(!accepted)),
            "{label}: {stderr}"
        );
        assert!(stderr.contains(diagnostic), "{label}: {stderr}");
        eprintln!(
            "{label}: compile=0, scan accepted={accepted}, actual policy={} {stderr}",
            output.status.code().unwrap()
        );
    }
}

fn macro_source() -> &'static str {
    "extern crate proc_macro;
    fn generated() -> proc_macro::TokenStream {
        let mut source = String::from(\"pub fn generated() {\\n\");
        source.push_str(&\"let _value = 1;\\n\".repeat(900));
        source.push_str(\"}\\n\");
        source.parse().unwrap()
    }
    #[proc_macro_attribute]
    pub fn serde(_: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
        let mut output = item; output.extend(generated()); output
    }
    #[proc_macro_derive(Debug)]
    pub fn debug(_: proc_macro::TokenStream) -> proc_macro::TokenStream { generated() }
    #[proc_macro_derive(Serialize)]
    pub fn serialize(_: proc_macro::TokenStream) -> proc_macro::TokenStream { generated() }
    #[proc_macro_derive(Error)]
    pub fn error(_: proc_macro::TokenStream) -> proc_macro::TokenStream { generated() }
    "
}
