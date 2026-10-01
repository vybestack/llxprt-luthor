use std::{collections::BTreeMap, fs};
use tempfile::tempdir;
use xtask::{
    ledger, measurements,
    metrics::Limits,
    scan::{self, Scan},
};

fn scanned(files: &[(&str, &str)]) -> Result<Scan, String> {
    let root = tempdir().unwrap();
    for (path, source) in files {
        let path = root.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    let roots = if files[0].0.starts_with("xtask/") {
        vec!["xtask/src"]
    } else {
        vec!["src"]
    };
    scan::scan(root.path(), &roots)
}

fn methods(start: usize, count: usize) -> String {
    (start..start + count)
        .map(|i| format!("fn m{i}() {{}} "))
        .collect()
}

fn type_values(scan: &Scan, metric: &str) -> BTreeMap<String, usize> {
    measurements::collect(scan, Limits::default())
        .into_iter()
        .filter(|m| m.key.ends_with(metric))
        .map(|m| (m.key, m.value))
        .collect()
}

#[test]
fn ordinary_local_methods_reach_collection_and_ledger_at_twenty_and_twenty_one() {
    for prefix in ["src", "xtask/src"] {
        for count in [20, 21] {
            let calls = (0..count)
                .map(|i| format!("T::m{i}(); "))
                .collect::<String>();
            let source = format!(
                "fn run() {{ struct T; impl T {{ {} }} {calls} }}",
                methods(0, count)
            );
            let scan = scanned(&[(&format!("{prefix}/lib.rs"), &source)]).unwrap();
            let values = type_values(&scan, ":type_methods");
            assert_eq!(values.len(), 1, "{values:?}");
            assert_eq!(*values.values().next().unwrap(), count);
            let measured = measurements::collect(&scan, Limits::default());
            assert!(
                measured
                    .iter()
                    .any(|m| m.key.ends_with("T:type_methods") && m.limit == 20)
            );
            let errors = ledger::validate(&measured, &[]);
            if count == 20 {
                assert!(errors.is_empty(), "{errors:?}");
            } else {
                assert!(
                    errors
                        .iter()
                        .any(|e| e.contains("T:type_methods: type_methods=21")
                            && e.contains("limit=20")),
                    "{errors:?}"
                );
            }
        }
    }
}

#[test]
fn local_type_effective_lines_are_inclusive_at_four_hundred() {
    for total in [400, 401] {
        let mut methods = String::new();
        for i in 0..10 {
            let lines = 40 + usize::from(total == 401 && i == 0);
            methods.push_str(&format!(
                "fn m{i}() {{\n// not effective\n{} }}\n",
                "let _value = 1;\n".repeat(lines - 2)
            ));
        }
        let source = format!("fn run() {{ struct T; impl T {{\n{methods}}} }}");
        let scan = scanned(&[("src/lib.rs", &source)]).unwrap();
        let values = type_values(&scan, ":type_lines");
        assert_eq!(values.len(), 1, "{values:?}");
        assert_eq!(*values.values().next().unwrap(), total);
        let measured = measurements::collect(&scan, Limits::default());
        assert!(
            measured
                .iter()
                .any(|m| m.key.ends_with("T:type_lines") && m.limit == 400)
        );
        let errors = ledger::validate(&measured, &[]);
        let type_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.contains(":type_lines:"))
            .collect();
        assert_eq!(type_errors.len(), usize::from(total == 401), "{errors:?}");
        if total == 401 {
            assert!(type_errors[0].contains("type_lines=401 limit=400"));
        }
    }
}

#[test]
fn identical_names_in_functions_sibling_blocks_and_macros_are_distinct() {
    let body = methods(0, 20);
    for source in [
        format!(
            "fn a() {{ struct T; impl T {{ {body} }} }} fn b() {{ struct T; impl T {{ {body} }} }}"
        ),
        format!(
            "fn a() {{ {{ struct T; impl T {{ {body} }} }} {{ struct T; impl T {{ {body} }} }} }}"
        ),
        format!(
            "fn a() {{ let _ = vec![{{ struct T; impl T {{ {body} }} 0 }}, {{ struct T; impl T {{ {body} }} 0 }}]; }}"
        ),
        format!("struct T; impl T {{ {body} }} fn a() {{ struct T; impl T {{ {body} }} }}"),
    ] {
        let scan = scanned(&[("src/lib.rs", &source)]).unwrap();
        let values = type_values(&scan, ":type_methods");
        assert_eq!(values.len(), 2, "{values:?}");
        assert!(values.values().all(|v| *v == 20), "{values:?}");
        assert!(ledger::validate(&measurements::collect(&scan, Limits::default()), &[]).is_empty());
    }
}

#[test]
fn split_impls_nested_functions_and_macro_blocks_share_the_declared_local_type() {
    for nested in [
        format!("{{ impl T {{ {} }} }}", methods(10, 11)),
        format!("fn nested() {{ impl T {{ {} }} }}", methods(10, 11)),
        format!("let _ = vec![{{ impl T {{ {} }} 0 }}];", methods(10, 11)),
    ] {
        let source = format!(
            "fn run() {{ impl T {{ {} }} struct T; {nested} }}",
            methods(0, 10)
        );
        let scan = scanned(&[("src/lib.rs", &source)]).unwrap();
        let values = type_values(&scan, ":type_methods");
        assert_eq!(values.len(), 1, "{values:?}");
        assert_eq!(*values.values().next().unwrap(), 21);
        assert!(
            ledger::validate(&measurements::collect(&scan, Limits::default()), &[])
                .iter()
                .any(|e| e.contains("type_methods=21 limit=20"))
        );
    }
}

#[test]
fn local_shadowing_and_block_import_aliases_resolve_to_the_correct_declaration() {
    let source = format!(
        "mod global {{ pub struct T; impl T {{ {} }} }} fn run() {{ use crate::global::T as Global; struct T; impl T {{ {} }} {{ impl T {{ {} }} impl Global {{ {} }} }} }}",
        methods(0, 10),
        methods(0, 10),
        methods(10, 10),
        methods(10, 11),
    );
    let scan = scanned(&[("src/lib.rs", &source)]).unwrap();
    let values = type_values(&scan, ":type_methods");
    assert_eq!(values.len(), 2, "{values:?}");
    assert_eq!(values["src/lib.rs::global::T:type_methods"], 21);
    assert!(
        values
            .iter()
            .any(|(key, count)| key != "src/lib.rs::global::T:type_methods" && *count == 20)
    );
}

#[test]
fn cross_file_global_aliases_used_inside_functions_keep_one_aggregate() {
    let a = format!("pub struct T; impl T {{ {} }}", methods(0, 10));
    let b = format!(
        "use crate::a::T as Alias; fn run() {{ impl Alias {{ {} }} }}",
        methods(10, 11)
    );
    let scan = scanned(&[
        ("src/lib.rs", "mod a; mod b;"),
        ("src/a.rs", &a),
        ("src/b.rs", &b),
    ])
    .unwrap();
    let values = type_values(&scan, ":type_methods");
    assert_eq!(
        values,
        BTreeMap::from([("src/a.rs::a::T:type_methods".into(), 21)])
    );
    assert!(
        ledger::validate(&measurements::collect(&scan, Limits::default()), &[])
            .iter()
            .any(|e| e.contains("type_methods=21 limit=20"))
    );
}

#[test]
fn local_type_alias_chains_cannot_split_one_types_responsibilities() {
    let source = format!(
        "fn run() {{ struct T; type Alias = T; type Again = Alias; impl T {{ {} }} {{ impl Again {{ {} }} }} }}",
        methods(0, 10),
        methods(10, 11)
    );
    let scan = scanned(&[("src/lib.rs", &source)]).unwrap();
    let values = type_values(&scan, ":type_methods");
    assert_eq!(values.len(), 1);
    assert_eq!(*values.values().next().unwrap(), 21);
}

#[test]
fn nested_types_do_not_count_as_methods_of_the_enclosing_type() {
    let source = format!(
        "struct Outer; impl Outer {{ fn run() {{ struct T; impl T {{ {} }} fn free() {{}} }} }}",
        methods(0, 20)
    );
    let scan = scanned(&[("src/lib.rs", &source)]).unwrap();
    let values = type_values(&scan, ":type_methods");
    assert_eq!(values.len(), 2);
    assert_eq!(values["src/lib.rs::root::Outer:type_methods"], 1);
    assert!(values.values().any(|v| *v == 20));
    assert!(ledger::validate(&measurements::collect(&scan, Limits::default()), &[]).is_empty());
}

#[test]
fn unresolved_or_unsupported_local_scope_resolution_fails_the_scan() {
    for source in [
        "fn run() { impl Missing { fn method() {} } }",
        "fn run() { { struct T; } { impl T { fn method() {} } } }",
        "fn run() { struct T; type Alias = (T,); impl Alias { fn method() {} } }",
        "fn run() { type A = B; type B = A; impl A { fn method() {} } }",
        "fn run() { mod nested { pub struct T; } impl nested::T { fn method() {} } }",
        "fn run() { struct T; impl <T as Trait>::Associated { fn method() {} } }",
        "fn run() { struct T; struct T; impl T { fn method() {} } }",
        "fn run() { struct T; use missing::*; impl Imported { fn method() {} } }",
        "fn run() { struct T; type Alias<T> = T; impl Alias<T> { fn method() {} } }",
        "mod T { pub struct Inner; } fn run() { struct T; impl T::Inner { fn method() {} } }",
    ] {
        let error = scanned(&[("src/lib.rs", source)]).unwrap_err();
        assert!(error.contains("src/lib.rs"), "{source}: {error}");
        assert!(
            error.contains("cannot aggregate safely"),
            "{source}: {error}"
        );
    }
}

#[test]
fn type_aliases_bind_in_their_declaration_scope_not_the_impl_scope() {
    let source = format!(
        "fn run() {{ struct T; type Alias = T; impl T {{ {} }} {{ struct T; impl T {{ {} }} impl Alias {{ {} }} }} }}",
        methods(0, 10),
        methods(0, 20),
        methods(10, 11),
    );
    let scan = scanned(&[("src/lib.rs", &source)]).unwrap();
    let values = type_values(&scan, ":type_methods");
    assert_eq!(values.len(), 2, "{values:?}");
    assert_eq!(
        values
            .values()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [20, 21].into()
    );
}

#[test]
fn local_traits_enums_and_generic_impls_use_scoped_type_totals() {
    let source = format!(
        "fn run() {{ enum T<X> {{ Value(X) }} impl<X> T<X> {{ {} }} {{ impl T<u8> {{ {} }} }} trait Q {{ {} }} }}",
        methods(0, 10),
        methods(10, 11),
        methods(0, 20),
    );
    let scan = scanned(&[("src/lib.rs", &source)]).unwrap();
    let values = type_values(&scan, ":type_methods");
    assert_eq!(values.len(), 2);
    assert!(
        values
            .iter()
            .any(|(k, v)| k.ends_with("T:type_methods") && *v == 21)
    );
    assert!(
        values
            .iter()
            .any(|(k, v)| k.ends_with("Q:type_methods") && *v == 20)
    );
}

#[test]
fn global_type_alias_chains_inside_local_impls_keep_the_declaring_type_key() {
    let a = format!("pub struct T; impl T {{ {} }}", methods(0, 10));
    let b = format!(
        "type Alias = crate::a::T; type Again = Alias; fn run() {{ impl Again {{ {} }} }}",
        methods(10, 11)
    );
    let scan = scanned(&[
        ("src/lib.rs", "mod a; mod b;"),
        ("src/a.rs", &a),
        ("src/b.rs", &b),
    ])
    .unwrap();
    assert_eq!(
        type_values(&scan, ":type_methods"),
        BTreeMap::from([("src/a.rs::a::T:type_methods".into(), 21)])
    );
}

#[test]
fn split_local_type_lines_across_sibling_impl_blocks_cannot_evade_the_cap() {
    let body = "let _value = 1;\n".repeat(38);
    let first = (0..5)
        .map(|i| format!("fn m{i}() {{\n{body}}}\n"))
        .collect::<String>();
    let second = (5..10)
        .map(|i| format!("fn m{i}() {{\n{body}}}\n"))
        .collect::<String>();
    let source = format!(
        "fn run() {{ struct T; {{ impl T {{ {first} }} }} {{ impl T {{ {second} fn extra() {{}} }} }} }}"
    );
    let scan = scanned(&[("src/lib.rs", &source)]).unwrap();
    assert_eq!(
        type_values(&scan, ":type_lines")
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [401]
    );
    assert!(
        ledger::validate(&measurements::collect(&scan, Limits::default()), &[])
            .iter()
            .any(|e| e.contains("T:type_lines: type_lines=401 limit=400"))
    );
}

#[test]
fn unqualified_block_use_aliases_bind_to_the_local_type_and_its_declaration_scope() {
    for alias in ["use T as Alias;", "use T as First; use First as Alias;"] {
        let source = format!(
            "struct T; fn run() {{ struct T; {alias} impl T {{ {} }} {{ impl Alias {{ {} }} }} }}",
            methods(0, 20),
            methods(20, 1),
        );
        let scan = scanned(&[("src/lib.rs", &source)]).unwrap();
        let values = type_values(&scan, ":type_methods");
        assert_eq!(values.len(), 1, "{values:?}");
        assert_eq!(*values.values().next().unwrap(), 21, "{values:?}");
        assert!(values.keys().all(|key| key.contains("::local@")));
        assert!(
            ledger::validate(&measurements::collect(&scan, Limits::default()), &[])
                .iter()
                .any(|e| e.contains("type_methods=21 limit=20"))
        );
    }
    let source = format!(
        "fn run() {{ struct T; use T as Alias; impl T {{ {} }} {{ struct T; impl T {{ {} }} impl Alias {{ {} }} }} }}",
        methods(0, 20),
        methods(0, 20),
        methods(20, 1),
    );
    let scan = scanned(&[("src/lib.rs", &source)]).unwrap();
    let values = type_values(&scan, ":type_methods");
    assert_eq!(values.len(), 2, "{values:?}");
    assert_eq!(
        values
            .values()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [20, 21].into()
    );
}
