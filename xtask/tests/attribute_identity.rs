#[test]
fn compiled_raw_builtin_spellings_cannot_hide_suppressions() {
    let fixture = Fixture::new();
    for (label, source) in [
        ("raw allow", "#[r#allow(warnings)] pub struct Record;"),
        ("raw expect", "#[r#expect(dead_code)] struct Record;"),
        (
            "raw cfg_attr suppression",
            "#[r#cfg_attr(any(), r#allow(warnings))] pub struct Record;",
        ),
        (
            "nested raw cfg_attr suppression",
            "#[cfg_attr(any(), r#cfg_attr(all(), r#expect(unused)))] pub struct Record;",
        ),
        (
            "raw suppression inside macro",
            "pub fn run() { assert!({ #[r#allow(warnings)] let x = true; x }); }",
        ),
    ] {
        if label == "raw suppression inside macro" {
            fixture.compiled(label, source, false, "forbidden lint suppression");
        } else {
            fixture.suppressed(label, source);
        }
    }
}

mod attribute_support;
use attribute_support::Fixture;
use std::fs;

#[test]
fn compiled_error_helpers_reject_executable_arguments_and_hidden_suppression() {
    let fixture = Fixture::new();
    for (label, expression) in [
        (
            "900 executable helper lines",
            format!(
                "{{\nlet mut count = 0;\n{}count\n}}",
                "count += 1;\n".repeat(900)
            ),
        ),
        (
            "nested allow helper",
            "{ #[allow(warnings)] let unused = 1; 123 }".into(),
        ),
        (
            "included executable helper",
            "include!(\"hidden.txt\")".into(),
        ),
        ("extra literal formatting argument", "123".into()),
    ] {
        fs::write(
            fixture.root.join("src/hidden.txt"),
            format!(
                "{{
let mut count = 0;
{}count
}}",
                "count += 1;
"
                .repeat(900)
            ),
        )
        .unwrap();
        fixture.compiled(label, &format!("#[derive(Debug, thiserror::Error)]\npub enum Fault {{\n#[error(\"{{}}\", {expression})]\nFailure,\n}}\n"), false, "unsupported executable/extended error helper");
    }
    fixture.compiled(
        "literal and transparent helpers",
        "#[derive(Debug, thiserror::Error)] pub enum Fault {
        #[error(\"failure {0}\")] Literal(u8),
        #[error(transparent)] Io(#[from] std::io::Error),
    }",
        true,
        "",
    );
}

#[test]
fn compiled_procedural_names_aliases_and_reexports_are_not_builtin_derives() {
    let fixture = Fixture::new();
    for (label, source, diagnostic) in [
        (
            "procedural serde on fn",
            "use fixture_macro::serde; #[serde] pub fn small() {}",
            "unverified helper context",
        ),
        (
            "inactive derive cannot authorize procedural helper",
            "use fixture_macro::serde; #[cfg_attr(any(), derive(serde::Serialize))] #[serde] pub struct Small;",
            "unverified helper context",
        ),
        (
            "procedural Debug",
            "use fixture_macro::Debug; #[derive(Debug)] pub struct Small;",
            "derive",
        ),
        (
            "Debug renamed Serialize",
            "use fixture_macro::Debug as Serialize; #[derive(Serialize)] pub struct Small;",
            "derive",
        ),
        (
            "qualified procedural Debug",
            "#[derive(fixture_macro::Debug)] pub struct Small;",
            "derive",
        ),
        (
            "local reexport Debug",
            "mod exports { pub use fixture_macro::Debug; } #[derive(exports::Debug)] pub struct Small;",
            "derive",
        ),
        (
            "block-local alias",
            "pub fn small() { use fixture_macro::Debug as Serialize; #[derive(Serialize)] struct Local; }",
            "derive",
        ),
        (
            "serde namespace alias",
            "use fixture_macro as serde; #[derive(serde::Serialize)] pub struct Small;",
            "derive",
        ),
        (
            "core namespace shadow",
            "mod core { pub mod fmt { pub use fixture_macro::Debug; } } #[derive(core::fmt::Debug)] pub struct Small;",
            "derive",
        ),
        (
            "extern core alias",
            "extern crate fixture_macro as core; #[derive(::core::Debug)] pub struct Small;",
            "derive",
        ),
        (
            "glob procedural Debug",
            "use fixture_macro::*; #[derive(Serialize)] pub struct Small;",
            "glob derive binding",
        ),
        (
            "inactive procedural derive",
            "#[cfg_attr(any(), derive(fixture_macro::Debug))] pub struct Small;",
            "derive",
        ),
    ] {
        fixture.compiled(label, source, false, diagnostic);
    }
    fs::write(
        fixture.root.join("src/exports.rs"),
        "pub use fixture_macro::Debug as Serialize;",
    )
    .unwrap();
    fixture.compiled(
        "external module reexport",
        "mod exports; #[derive(exports::Serialize)] pub struct Small;",
        false,
        "derive",
    );
}

#[test]
fn compiled_raw_qualified_builtins_and_registry_aliases_keep_metadata_exemption() {
    let fixture = Fixture::new();
    for (label, source) in [
        (
            "raw repr and doc",
            "#[r#repr(C)] #[r#doc = \"record\"] pub struct Record { pub value: u8 }",
        ),
        ("raw Debug", "#[derive(r#Debug)] pub struct Record;"),
        (
            "qualified builtin derives",
            "#[derive(core::fmt::Debug, core::clone::Clone)] pub struct Record;",
        ),
        ("used static", "#[used] pub static VALUE: u8 = 1;"),
        ("no_std crate", "#![no_std]\npub struct Record;"),
        (
            "unsafe no_mangle",
            "#[unsafe(no_mangle)] pub extern \"C\" fn exported() {}",
        ),
        (
            "builtin alias",
            "use core::fmt::Debug as Diagnostic; #[derive(Diagnostic)] pub struct Record;",
        ),
        (
            "absolute core bypasses local module",
            "mod core { pub mod fmt { pub use fixture_macro::Debug; } } #[derive(::core::fmt::Debug)] pub struct Record;",
        ),
        (
            "literal serde helper",
            "#[derive(serde::Serialize, serde::Deserialize)] #[serde(deny_unknown_fields)] pub struct Record { #[serde(default, rename = \"v\")] pub value: u8 }",
        ),
        (
            "local registry reexports",
            "mod exports { pub use serde::Serialize as Encode; pub use thiserror::Error as Failure; } #[derive(exports::Encode)] pub struct Record; #[derive(Debug, exports::Failure)] #[error(\"failure\")] pub struct Fault;",
        ),
        (
            "inactive helper metadata",
            "#[derive(serde::Serialize)] #[cfg_attr(any(), serde(rename_all = \"snake_case\"))] pub struct Record;",
        ),
        (
            "conditional derive and helper share branch",
            "#[cfg_attr(all(), derive(serde::Serialize), serde(rename_all = \"snake_case\"))] pub struct Record;",
        ),
    ] {
        fixture.compiled(label, source, true, "");
    }
    fixture.manifest(
        "serde = { version = \"1\", features = [\"derive\"] }",
        "serialization = { package = \"serde\", version = \"1\", features = [\"derive\"] }",
    );
    fixture.manifest(
        "thiserror = \"2\"",
        "errors = { package = \"thiserror\", version = \"2\" }",
    );
    fixture.compiled("renamed registry dependencies", "extern crate errors as thiserror; use errors::Error as Failure; #[derive(Debug, Failure)] #[error(\"failure\")] pub struct Fault;", true, "");
    // Serialize needs the explicit helper crate when its Cargo dependency is renamed.
    fixture.compiled("renamed serde registry dependency", "#[derive(serialization::Serialize)] #[serde(crate = \"serialization\")] pub struct Record;", true, "");
    fs::write(
        fixture.root.join("src/exports.rs"),
        "pub use serialization::Serialize as Encode;",
    )
    .unwrap();
    fixture.compiled("external registry reexport", "mod exports; #[derive(exports::Encode)] #[serde(crate = \"serialization\")] pub struct Record;", true, "");
}

fn replacement(fixture: &Fixture, name: &str, version: &str, source: &str, extra: &str) {
    let path = fixture.root.join(format!("tmp/{name}"));
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("Cargo.toml"), format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2024\"\n[workspace]\n{extra}")).unwrap();
    fs::write(path.join("src/lib.rs"), source).unwrap();
}

#[test]
fn compiled_path_and_transitive_replacements_cannot_claim_registry_identity() {
    let fixture = Fixture::new();
    replacement(
        &fixture,
        "serde",
        "1.0.0",
        "pub use fixture_macro::Serialize;",
        "[dependencies]\nfixture_macro = { path = \"../macro\" }\n",
    );
    fixture.manifest(
        "serde = { version = \"1\", features = [\"derive\"] }",
        "serde = { path = \"tmp/serde\" }",
    );
    fixture.compiled(
        "path replacement serde",
        "#[derive(serde::Serialize)] pub struct Small;",
        false,
        "path/git replacement",
    );
    fixture.manifest(
        "serde = { path = \"tmp/serde\" }",
        "serde = { version = \"1\", features = [\"derive\"] }",
    );
    replacement(
        &fixture,
        "thiserror",
        "2.0.0",
        "pub use fixture_macro::Error;",
        "[dependencies]\nfixture_macro = { path = \"../macro\" }\n",
    );
    fixture.manifest(
        "thiserror = \"2\"",
        "thiserror = { path = \"tmp/thiserror\" }",
    );
    fixture.compiled(
        "path replacement thiserror",
        "#[derive(thiserror::Error)] pub struct Small;",
        false,
        "path/git replacement",
    );
}

#[test]
fn compiled_builtin_namespace_and_transitive_replacements_fail_closed() {
    let fixture = Fixture::new();
    replacement(
        &fixture,
        "core",
        "0.1.0",
        "pub mod fmt { pub use fixture_macro::Debug; }",
        "[dependencies]\nfixture_macro = { path = \"../macro\" }\n",
    );
    fixture.manifest(
        "thiserror = \"2\"",
        "thiserror = \"2\"\ncore = { path = \"tmp/core\" }",
    );
    fixture.compiled(
        "dependency replacing core",
        "#[derive(::core::fmt::Debug)] pub struct Small;",
        false,
        "namespace core is replaced",
    );
    fixture.manifest("core = { path = \"tmp/core\" }", "");
    let metadata: serde_json::Value = serde_json::from_slice(
        &fixture
            .cargo()
            .args(["metadata", "--offline", "--locked", "--format-version", "1"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let version = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "serde_derive")
        .unwrap()["version"]
        .as_str()
        .unwrap();
    replacement(
        &fixture,
        "serde_derive",
        version,
        "extern crate proc_macro; #[proc_macro_derive(Serialize)] pub fn serialize(_: proc_macro::TokenStream) -> proc_macro::TokenStream { \"pub fn generated() {}\".parse().unwrap() } #[proc_macro_derive(Deserialize)] pub fn deserialize(_: proc_macro::TokenStream) -> proc_macro::TokenStream { proc_macro::TokenStream::new() }",
        "[lib]\nproc-macro = true\n",
    );
    fixture.manifest(
        "thiserror = \"2\"",
        "thiserror = \"2\"\n[patch.crates-io]\nserde_derive = { path = \"tmp/serde_derive\" }",
    );
    fixture.compiled(
        "transitive serde_derive replacement",
        "#[derive(serde::Serialize)] pub struct Small;",
        false,
        "path/git replacement",
    );
}

#[test]
fn standalone_measurement_requires_verified_helpers_and_resolved_bindings() {
    for source in [
        "#[serde(default)] fn small() {}",
        "#[error(\"small\")] struct Small;",
        "#[derive(serde::Serialize)] struct Small;",
        "use fixture_macro::Debug; #[derive(Debug)] struct Small;",
        "mod exports {} #[derive(exports::Debug)] struct Small;",
        "#[cfg_attr(any(), serde(default))] struct Small;",
        "#[derive(Debug)] struct Small { #[from] field: u8 }",
    ] {
        assert!(
            xtask::metrics::analyze("src/lib.rs", source).is_err(),
            "{source}"
        );
    }
}
