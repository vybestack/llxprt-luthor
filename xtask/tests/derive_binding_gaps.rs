mod attribute_support;
use attribute_support::Fixture;
use std::{fs, process::Command};

fn executable_generator(fixture: &Fixture) {
    fs::write(fixture.root.join("tmp/macro/src/lib.rs"), "extern crate proc_macro;
fn generated() -> proc_macro::TokenStream {
    let mut source = String::from(\"pub fn generated() -> u32 {\\nlet mut count = 0;\\n\");
    source.push_str(&\"count += 1;\\n\".repeat(900));
    source.push_str(\"count\\n}\\n\");
    source.parse().unwrap()
}
#[proc_macro_attribute]
pub fn serde(_: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let mut output = item; output.extend(generated()); output
}
#[proc_macro_attribute]
pub fn derive(_: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let mut output = item; output.extend(generated()); output
}
#[proc_macro_derive(Debug)]
pub fn debug(_: proc_macro::TokenStream) -> proc_macro::TokenStream { generated() }
").unwrap();
}

fn runtime(fixture: &Fixture, label: &str, function: &str) {
    fs::write(
        fixture.root.join("tests/smoke.rs"),
        format!("#[test] fn smoke() {{ assert_eq!(luthor::{function}(), 900); }}\n"),
    )
    .unwrap();
    let output = Command::new("cargo")
        .args([
            "test",
            "-p",
            "luthor",
            "--test",
            "smoke",
            "--all-features",
            "--offline",
            "--locked",
        ])
        .current_dir(&fixture.root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{label}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    eprintln!("{label}: generated executable statements=900, runtime=0");
    fs::write(
        fixture.root.join("tests/smoke.rs"),
        "#[test] fn smoke() {}\n",
    )
    .unwrap();
}

#[test]
fn compiled_procedural_derive_attributes_fail_closed() {
    let fixture = Fixture::new();
    executable_generator(&fixture);
    for (label, source, function) in [
        (
            "procedural attribute named derive",
            "use fixture_macro::derive;\n#[derive(Debug)]\npub struct Record;\n",
            "generated",
        ),
        (
            "procedural serde attribute renamed derive",
            "use fixture_macro::serde as derive;\n#[derive(Debug)]\npub struct Record;\n",
            "generated",
        ),
        (
            "conditional procedural derive attribute",
            "use fixture_macro::serde as derive;\n#[cfg_attr(all(), derive(Debug))]\npub struct Record;\n",
            "generated",
        ),
        (
            "block-local procedural derive attribute",
            "pub fn run() -> u32 { use fixture_macro::serde as derive; #[derive(Debug)] struct Record; generated() }",
            "run",
        ),
    ] {
        fixture.compiled(label, source, false, "derive attribute binding");
        assert!(xtask::metrics::analyze("src/lib.rs", source).is_err());
        runtime(&fixture, label, function);
    }
    fixture.compiled(
        "compiler builtin derive attribute",
        "#[derive(Debug)]\npub struct Record;\n",
        true,
        "",
    );
}

fn metadata(fixture: &Fixture, features: &[&str]) -> serde_json::Value {
    let output = Command::new("cargo")
        .args(["metadata", "--offline", "--locked", "--format-version", "1"])
        .args(features)
        .current_dir(&fixture.root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn core_dependency(metadata: &serde_json::Value) -> bool {
    metadata["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| {
            node["deps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|dep| dep["name"] == "core")
        })
}

#[test]
fn compiled_optional_core_alias_is_not_the_compiler_namespace() {
    let fixture = Fixture::new();
    executable_generator(&fixture);
    let path = fixture.root.join("tmp/fakecore");
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("Cargo.toml"), "[package]\nname = \"fakecore\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n[dependencies]\nfixture_macro = { path = \"../macro\" }\n").unwrap();
    fs::write(
        path.join("src/lib.rs"),
        "pub mod fmt { pub use fixture_macro::Debug; }\n",
    )
    .unwrap();
    fixture.manifest("\"tmp/core\"", "\"tmp/core\", \"tmp/fakecore\"");
    fixture.manifest(
        "fixture_macro = { path = \"tmp/macro\" }",
        "fixture_macro = { path = \"tmp/macro\" }\ncore = { package = \"fakecore\", path = \"tmp/fakecore\", optional = true }",
    );
    assert!(!core_dependency(&metadata(&fixture, &[])));
    assert!(core_dependency(&metadata(&fixture, &["--all-features"])));
    eprintln!("optional core: absent in default resolution, present in all-features resolution");
    fixture.compiled(
        "optional package alias replacing core",
        "#[derive(::core::fmt::Debug)]\npub struct Record;\n",
        false,
        "namespace core is replaced",
    );
    runtime(
        &fixture,
        "optional package alias replacing core",
        "generated",
    );
}

#[test]
fn compiled_raw_module_reexports_and_nested_derives_keep_builtin_identity() {
    let fixture = Fixture::new();
    for (label, source) in [
        (
            "raw module standard derive reexport",
            "mod r#type { pub use std::fmt::Debug as D; }\n#[derive(r#type::D)]\npub struct Record;\n",
        ),
        (
            "raw module nested builtin derive",
            "mod r#type { #[derive(Debug)] pub struct Record; }\n",
        ),
        (
            "nested raw module builtin derive",
            "mod r#type { mod r#match { #[derive(Debug)] pub struct Record; } }\n",
        ),
    ] {
        fixture.compiled(label, source, true, "");
        let report = xtask::metrics::analyze("src/lib.rs", source).unwrap();
        eprintln!("{label}: standalone file_lines={}", report.file_lines);
    }
    let integration = "mod r#type { #[derive(Debug)] pub struct Record; }\n#[test] fn smoke() { let _ = r#type::Record; }\n";
    fs::write(fixture.root.join("tests/smoke.rs"), integration).unwrap();
    assert!(xtask::metrics::analyze("tests/smoke.rs", integration).is_ok());
    fixture.compiled(
        "raw module in integration test",
        "#[derive(Debug)] pub struct Record;\n",
        true,
        "",
    );
    fixture.compiled(
        "raw module procedural derive reexport",
        "mod r#type { pub use fixture_macro::Debug as D; }\n#[derive(r#type::D)]\npub struct Record;\n",
        false,
        "path/git replacement",
    );
    fixture.suppressed(
        "raw module suppression still rejected",
        "mod r#type { #[allow(warnings)] pub struct Record; }\n",
    );
}
