use std::fs;
use tempfile::tempdir;
use xtask::scan::scan;

#[test]
fn required_roots_nested_sources_and_malformed_input_fail_closed() {
    let root = tempdir().unwrap();
    assert!(scan(root.path(), &["src"]).is_err());
    fs::create_dir_all(root.path().join("src/nested")).unwrap();
    fs::write(
        root.path().join("src/lib.rs"),
        "pub mod nested { pub mod b; }",
    )
    .unwrap();
    fs::write(root.path().join("src/nested/b.rs"), "pub fn b() {}").unwrap();
    assert_eq!(scan(root.path(), &["src"]).unwrap().reports.len(), 2);
    fs::write(root.path().join("src/nested/b.rs"), "fn broken(").unwrap();
    assert!(
        scan(root.path(), &["src"])
            .unwrap_err()
            .contains("src/nested/b.rs")
    );
}

#[cfg(unix)]
#[test]
fn symlinks_and_unreadable_inputs_are_not_silently_skipped() {
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(root.path().join("outside.rs"), "fn a() {}").unwrap();
    symlink(
        root.path().join("outside.rs"),
        root.path().join("src/lib.rs"),
    )
    .unwrap();
    assert!(scan(root.path(), &["src"]).is_err());
}

#[test]
fn cross_file_impls_aggregate_under_the_declaring_type() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(root.path().join("src/lib.rs"), "pub mod a; pub mod b;").unwrap();
    fs::write(
        root.path().join("src/a.rs"),
        "pub struct T; impl T { pub fn a() {} }",
    )
    .unwrap();
    fs::write(
        root.path().join("src/b.rs"),
        "use crate::a::T as Alias; impl Alias { pub fn b() {} }",
    )
    .unwrap();
    let result = scan(root.path(), &["src"]).unwrap();
    assert!(result.types.values().any(|(_, methods)| *methods == 2));
}

#[test]
fn missing_or_unreachable_module_is_incomplete_enumeration() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(root.path().join("src/lib.rs"), "mod missing;").unwrap();
    assert!(scan(root.path(), &["src"]).is_err());
    fs::write(root.path().join("src/lib.rs"), "").unwrap();
    fs::write(root.path().join("src/orphan.rs"), "fn f() {}").unwrap();
    assert!(scan(root.path(), &["src"]).is_err());
}

#[test]
fn nested_inline_module_cycle_is_not_hidden_in_its_parent() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(
        root.path().join("src/lib.rs"),
        "mod a { pub fn f() { crate::b::f(); } } mod b { pub fn f() { crate::a::f(); } }",
    )
    .unwrap();
    assert!(!scan(root.path(), &["src"]).unwrap().feedback.is_empty());
}

#[test]
fn missing_crate_entry_cannot_promote_an_orphan_into_production() {
    let root = tempdir().unwrap();
    fs::create_dir_all(root.path().join("src")).unwrap();
    fs::write(root.path().join("src/orphan.rs"), "fn hidden() {}").unwrap();
    let error = scan(root.path(), &["src"]).unwrap_err();
    assert!(error.contains("incomplete"), "{error}");
}

#[test]
fn empty_required_root_is_not_a_complete_scan_even_with_other_inputs() {
    let root = tempdir().unwrap();
    fs::create_dir_all(root.path().join("src")).unwrap();
    fs::create_dir_all(root.path().join("tests")).unwrap();
    fs::write(
        root.path().join("tests/sample.rs"),
        "#[test] fn passes() {}",
    )
    .unwrap();
    let error = scan(root.path(), &["src", "tests"]).unwrap_err();
    assert!(
        error.contains("src") && error.contains("incomplete"),
        "{error}"
    );
}
