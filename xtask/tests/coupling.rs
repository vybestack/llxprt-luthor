use std::collections::BTreeSet;
use xtask::coupling::{edges, feedback_edges};

#[test]
fn dependency_cycles_include_aliases_nested_imports_and_cfg_branches() {
    let modules = BTreeSet::from(["a".into(), "b".into(), "nested".into(), "nested::c".into()]);
    let a = edges(
        "a",
        "use crate::b::{Thing as Alias}; fn f(x: Alias) {}",
        &modules,
    )
    .unwrap();
    let b = edges("b", "#[cfg(windows)] fn f() { crate::a::run(); }", &modules).unwrap();
    let mut all = a;
    all.extend(b);
    assert_eq!(feedback_edges(&all).len(), 1);
    let nested = edges("nested::c", "use super::super::a::Thing;", &modules).unwrap();
    assert!(nested.contains(&("nested::c".into(), "a".into())));
    assert!(
        edges(
            "a",
            "// crate::b::run()\nfn f() { let s = \"crate::b::run()\"; }",
            &modules
        )
        .unwrap()
        .is_empty()
    );
    assert!(edges("a", "use crate::missing::Thing;", &modules).is_err());
    assert!(edges("a", "fn broken(", &modules).is_err());
}

#[test]
fn acyclic_edges_pass_and_new_feedback_edges_are_visible() {
    let graph = BTreeSet::from([("a".into(), "b".into()), ("b".into(), "c".into())]);
    assert!(feedback_edges(&graph).is_empty());
    let mut cycle = graph;
    cycle.insert(("c".into(), "a".into()));
    assert_eq!(
        feedback_edges(&cycle),
        BTreeSet::from([("c".into(), "a".into())])
    );
}

#[test]
fn sibling_qualified_paths_cannot_hide_a_cycle_without_crate_prefixes() {
    let modules = BTreeSet::from(["a".into(), "b".into(), "a::inner".into()]);
    let mut graph = edges("a", "fn f() { b::run(); }", &modules).unwrap();
    graph.extend(edges("b", "use a::Thing as T; fn f(_: T) {}", &modules).unwrap());
    assert_eq!(
        feedback_edges(&graph),
        BTreeSet::from([("b".into(), "a".into())])
    );
    assert_eq!(
        edges("a", "fn f() { inner::run(); std::mem::drop(1); }", &modules).unwrap(),
        BTreeSet::from([("a".into(), "a::inner".into())])
    );
}

#[test]
fn imported_module_aliases_cannot_hide_dependency_edges() {
    let modules = BTreeSet::from(["a".into(), "b".into()]);
    let edges = edges("a", "use b as B; fn f() { B::run(); }", &modules).unwrap();
    assert_eq!(edges, BTreeSet::from([("a".into(), "b".into())]));
}

#[test]
fn nested_generic_paths_are_scanned_in_both_directions() {
    let modules = BTreeSet::from(["a".into(), "b".into()]);
    let a = edges("a", "fn f(_: Option<crate::b::B>) {}", &modules).unwrap();
    let b = edges("b", "fn f(_: Option<Box<crate::a::A>>) {}", &modules).unwrap();
    assert!(a.contains(&("a".into(), "b".into())));
    assert!(b.contains(&("b".into(), "a".into())));
    let mut combined = a;
    combined.extend(b);
    assert_eq!(feedback_edges(&combined).len(), 1);
}
