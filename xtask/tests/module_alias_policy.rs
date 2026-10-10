use std::{fs, process::Command};
use xtask::{
    measurement::Measurement, measurements::collect, metrics::Limits, policy::validate, scan::scan,
};

fn fixture(source: &str) -> Result<Vec<Measurement>, String> {
    fixture_files(&[("src/lib.rs", source)])
}

fn fixture_files(files: &[(&str, &str)]) -> Result<Vec<Measurement>, String> {
    let root = tempfile::tempdir().unwrap();
    for (name, source) in files {
        let input = root.path().join(name);
        fs::create_dir_all(input.parent().unwrap()).unwrap();
        fs::write(input, source).unwrap();
    }
    for (entry, kind) in [("lib.rs", "lib"), ("main.rs", "bin")] {
        let input = root.path().join("src").join(entry);
        if !input.exists() {
            continue;
        }
        let output = Command::new("rustc")
            .args([
                "--edition=2024",
                "--crate-type",
                kind,
                "--emit=metadata",
                "--crate-name=alias_fixture",
            ])
            .arg(&input)
            .arg("-o")
            .arg(root.path().join(format!("{entry}.rmeta")))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    scan(root.path(), &["src"]).map(|scan| collect(&scan, Limits::default()))
}

fn cycle(import: &str, target: &str) -> String {
    format!(
        "pub mod a {{ pub mod nested {{ pub struct N {{ pub b: Option<crate::b::B> }} }} }}
        pub mod b {{ {import} pub struct B {{ pub n: Option<Box<{target}>> }} }}"
    )
}

fn assert_cycle_keys(findings: &[String], keys: &[&str]) {
    assert_eq!(findings.len(), keys.len(), "{findings:?}");
    for key in keys {
        let metric = key.rsplit(':').next().unwrap();
        assert!(
            findings.contains(&format!("{key}: {metric}=1 limit=0")),
            "{findings:?}"
        );
    }
}

fn assert_cycle(source: &str) {
    let measured = fixture(source).unwrap();
    assert_cycle_keys(
        &validate(&measured),
        &[
            "coupling::src/a::nested->src/b:cyclic_edge",
            "coupling::src/b->src/a::nested:feedback",
        ],
    );
    assert!(
        measured
            .iter()
            .filter(|m| m.key.starts_with("coupling::"))
            .all(|m| m.value == 1 && m.limit == 0)
    );
}

#[test]
fn nested_generic_cycles_are_rejected_for_direct_name_rename_and_group_imports() {
    for (import, target) in [
        ("", "crate::a::nested::N"),
        ("use crate::a as alias;", "alias::nested::N"),
        ("use crate::a;", "a::nested::N"),
        ("use crate::a::{self as alias};", "alias::nested::N"),
        ("use crate::{a as alias};", "alias::nested::N"),
        ("use crate::a::nested::N as Alias;", "Alias"),
    ] {
        assert_cycle(&cycle(import, target));
    }
}

#[test]
fn reexports_and_chained_parent_aliases_reach_the_defining_module() {
    let source = cycle("use crate::bridge::parent as alias;", "alias::nested::N");
    assert_cycle(&format!(
        "{source} pub mod bridge {{ pub use crate::a as parent; }}"
    ));
    for (import, target) in [
        ("use crate::bridge as alias;", "alias::N"),
        ("use crate::bridge::*;", "N"),
    ] {
        let source = cycle(import, target);
        let measured = fixture(&format!(
            "{source} pub mod bridge {{ pub use crate::a::nested::N; }}"
        ))
        .unwrap();
        assert_cycle_keys(
            &validate(&measured),
            &[
                "coupling::src/a::nested->src/b:cyclic_edge",
                "coupling::src/b->src/a::nested:feedback",
                "coupling::src/b->src/bridge:cyclic_edge",
                "coupling::src/bridge->src/a::nested:feedback",
            ],
        );
    }
}

#[test]
fn lexical_block_imports_are_hoisted_and_shadow_module_bindings() {
    let source = "pub mod a { pub mod nested { pub struct N { pub b: Option<crate::b::B> } } }
        pub mod b { use std as alias; pub struct B;
        pub fn f() { let _: Option<Box<alias::nested::N>> = None; use crate::a as alias; }
        pub fn g() { let _: Option<Box<alias::string::String>> = None; } }";
    assert_cycle(source);
}

#[test]
fn alias_cycle_replacement_is_rejected_and_breaking_the_cycle_passes() {
    let replaced = cycle("use crate::a as alias;", "alias::nested::N")
        .replace("mod a", "mod c")
        .replace("crate::a", "crate::c");
    assert_cycle_keys(
        &validate(&fixture(&replaced).unwrap()),
        &[
            "coupling::src/b->src/c::nested:cyclic_edge",
            "coupling::src/c::nested->src/b:feedback",
        ],
    );
    let acyclic = cycle("use crate::a as alias;", "alias::nested::N")
        .replace("pub b: Option<crate::b::B>", "pub value: usize");
    let measured = fixture(&acyclic).unwrap();
    assert!(validate(&measured).is_empty());
}

#[test]
fn external_and_nested_block_shadowing_do_not_invent_cycles() {
    let source = "pub mod a { pub mod nested { pub struct N { pub b: Option<crate::b::B> } } }
        pub mod c { pub mod nested { pub struct N; } }
        pub mod b { use crate::a as alias; pub struct B;
            pub fn f() { use crate::c as alias;
                let _: Option<alias::nested::N> = None;
                { use std as alias; let _: Option<alias::string::String> = None; }
                let _: Option<alias::nested::N> = None;
            }
        }";
    assert!(validate(&fixture(source).unwrap()).is_empty());
}

#[test]
fn external_files_and_parent_scope_aliases_resolve_before_cycle_accounting() {
    let measured = fixture_files(&[
        ("src/lib.rs", "pub mod a; pub mod b;"),
        ("src/a.rs", "pub mod nested { use super::super::b; pub struct N { pub b: Option<b::B> } }"),
        ("src/b.rs", "use crate::a as parent; use parent as alias; pub struct B { pub n: Option<Box<alias::nested::N>> }"),
    ]).unwrap();
    assert_eq!(validate(&measured).len(), 2);
    assert_cycle_keys(
        &validate(&measured),
        &[
            "coupling::src/a::nested->src/b:cyclic_edge",
            "coupling::src/b->src/a::nested:feedback",
        ],
    );
}

#[test]
fn import_targets_bind_in_their_declaration_scope() {
    assert_cycle(
        "pub mod a { pub mod nested { pub struct N { pub b: Option<crate::b::B> } } }
        pub mod c { pub mod nested { pub struct N; } }
        pub mod b { use crate::a as first; use first as alias; pub struct B;
            pub fn f() { use crate::c as first; let _: Option<Box<alias::nested::N>> = None;
                let _: Option<first::nested::N> = None; }
        }",
    );
}

#[test]
fn ambiguous_cfg_and_generic_alias_shadows_fail_closed_after_compilation() {
    let cfg = cycle(
        "#[cfg(any())] use crate::c as alias; #[cfg(not(any()))] use crate::a as alias;",
        "alias::nested::N",
    );
    let error = fixture(&format!(
        "{cfg} pub mod c {{ pub mod nested {{ pub struct N; }} }}"
    ))
    .unwrap_err();
    assert!(error.contains("ambiguous"), "{error}");
    let source = "pub mod a { pub mod nested { pub struct N; } }
        pub mod b { use crate::a as alias; pub fn f<alias>() {} }";
    let error = fixture(source).unwrap_err();
    assert!(error.contains("generic/module binding shadow"), "{error}");
}

#[test]
fn separate_binary_imports_do_not_shadow_library_module_names() {
    let source = cycle("use crate::a as alias;", "alias::nested::N");
    let measured = fixture_files(&[
        ("src/lib.rs", &source),
        (
            "src/main.rs",
            "use std as a; fn main() { let _: a::string::String = String::new(); }",
        ),
    ])
    .unwrap();
    assert_eq!(validate(&measured).len(), 2);
    assert_cycle_keys(
        &validate(&measured),
        &[
            "coupling::src/a::nested->src/b:cyclic_edge",
            "coupling::src/b->src/a::nested:feedback",
        ],
    );
}

#[test]
fn recursive_globs_and_block_module_identities_are_rejected_explicitly() {
    let source = "pub mod a { pub use crate::c::*; pub struct A; pub fn f(_: Option<A>) {} }
        pub mod c { pub use crate::a::*; pub struct C; }";
    let error = fixture(source).unwrap_err();
    assert!(
        error.contains("recursive/ambiguous module glob binding"),
        "{error}"
    );
    let source = "pub fn f() { mod local { pub struct N; } let _: Option<local::N> = None; }";
    let error = fixture(source).unwrap_err();
    assert!(error.contains("module"), "{error}");
}
