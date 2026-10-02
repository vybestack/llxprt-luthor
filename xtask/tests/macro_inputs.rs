use std::{collections::BTreeSet, fs};
use tempfile::tempdir;
use xtask::{
    coupling, ledger, measurements,
    metrics::{self, Limits},
    scan::{self, Scan},
    suppression,
};

fn scanned(source: &str) -> Result<Scan, String> {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(root.path().join("src/lib.rs"), source).unwrap();
    scan::scan(root.path(), &["src"])
}

fn findings(source: &str) -> Vec<String> {
    let scan = scanned(source).unwrap();
    ledger::validate(&measurements::collect(&scan, Limits::default()), &[])
}

fn costly_block() -> String {
    format!("{{ {} 0 }}", "operation()?; ".repeat(26))
}

#[test]
fn macro_branches_reach_the_ledger_at_the_existing_limit() {
    let block = costly_block();
    for invocation in [
        format!("vec![{block}]"),
        format!("vec![[(({block}))]]"),
        format!("vec![0; {block}]"),
        format!("params![{block}]"),
        format!("format!(\"{{value}}\", value = {block})"),
        format!("println!(\"{{}}\", {block})"),
        format!("assert!({block} == 0)"),
        format!("assert_eq!(0, {block})"),
        format!("matches!(0, _ if {block} == 0)"),
        format!("json!({{\"value\": [null, ({block})]}})"),
    ] {
        let source = format!(
            "fn operation() -> Result<(), ()> {{ Ok(()) }}
            fn run() -> Result<(), ()> {{ let _ = {invocation}; Ok(()) }}"
        );
        let errors = findings(&source);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("run:cyclomatic: cyclomatic=27") && e.contains("limit=25")),
            "{invocation}: {errors:?}"
        );
    }
}

#[test]
fn macro_local_impls_survive_production_measurement_collection() {
    let methods = (0..21)
        .map(|i| format!("fn m{i}() {{}} "))
        .collect::<String>();
    let source = format!("fn run() {{ let _ = vec![{{ struct T; impl T {{ {methods} }} 0 }}]; }}");
    let scan = scanned(&source).unwrap();
    assert_eq!(scan.reports[0].types["root::T"].1, 21);
    let measurements = measurements::collect(&scan, Limits::default());
    assert!(
        measurements
            .iter()
            .any(|m| m.key == "src/lib.rs::root::local@1:33::T:type_methods"
                && m.value == 21
                && m.limit == 20)
    );
    assert!(
        ledger::validate(&measurements, &[])
            .iter()
            .any(|e| e.contains("T:type_methods: type_methods=21") && e.contains("limit=20"))
    );
}

#[test]
fn macro_qualified_dependencies_reach_cycle_measurements() {
    for invocation in [
        "vec![crate::b::B]",
        "params![crate::b::B]",
        "format!(\"{:?}\", crate::b::B)",
        "assert!(matches!(crate::b::B, crate::b::B))",
        "matches!(0, crate::b::B)",
        "json!({\"value\": [crate::b::B]})",
    ] {
        let source = format!(
            "mod a {{ pub struct A; fn f() {{ let _ = {invocation}; }} }}
            mod b {{ pub struct B; fn f() {{ let _ = vec![crate::a::A]; }} }}"
        );
        let errors = findings(&source);
        assert!(
            errors.iter().any(|e| e.contains("coupling::")
                && e.contains("feedback=1")
                && e.contains("limit=0")),
            "{invocation}: {errors:?}"
        );
    }
}

#[test]
fn valid_under_limit_macros_and_ordinary_expect_methods_pass() {
    let source = r#"fn run() {
        let _ = vec![{ if ready() { 1 } else { 2 } }, 3,];
        let _ = vec![0; size()?];
        let _ = params![value.expect("value"), 2,];
        let _ = format!("{value}", value = 1);
        println!("{}", matches!(value, Some(x) | Other(x) if x > 0));
        assert!(true, "{}", 1);
        assert_eq!(1, 1, "{value}", value = 2);
        debug_assert_ne!(1, 2);
        write!(output, "{}", 1);
        writeln!(output);
        print!("hello"); eprint!("hello"); eprintln!(); println!();
        let _ = json!({"value": [null, true, {"nested": value.expect("value")}]});
        let _ = env!("CARGO_MANIFEST_DIR");
        let _ = option_env!("OPTIONAL");
        let _ = include_str!("data.txt");
        let _ = include_bytes!("data.txt");
        if ready() { unreachable!("{}", 1); }
    }"#;
    assert!(findings(source).is_empty());
}

#[test]
fn arbitrary_delimiters_and_unsupported_forms_fail_closed_in_all_entrypoints() {
    let modules = BTreeSet::from(["a".into(), "b".into()]);
    for invocation in [
        "vec![1; 2; 3]",
        "vec![1 2]",
        "params![1; 2]",
        r#"format!("{}", @@@)"#,
        r#"println!(("{}", { operation()?; 1 }))"#,
        r#"println!({ operation()?; "hello" })"#,
        "assert_eq!(1)",
        "assert!(true, custom!())",
        "matches!(value,)",
        "matches!(value, _ if)",
        r#"json!({"x": @@@})"#,
        "json!([1 2])",
        r#"json!({"x": 1} trailing)"#,
        r#"env!({ operation()?; "NAME" })"#,
        "unknown!({ operation()?; 1 })",
        "evil::vec![1]",
        "thread_local! { static X: i32 = 1; }",
        "Token![,]",
        "quote!(fn hidden() {})",
    ] {
        let source = format!("fn run() {{ let _ = {invocation}; }}");
        let error = scanned(&source).unwrap_err();
        assert!(
            error.contains("src/lib.rs") && error.contains("macro"),
            "{invocation}: {error}"
        );
        assert!(
            metrics::analyze("src/a.rs", &source).is_err(),
            "{invocation}"
        );
        assert!(
            coupling::edges("a", &source, &modules).is_err(),
            "{invocation}"
        );
        assert!(
            suppression::check("src/a.rs", &source).is_err(),
            "{invocation}"
        );
    }
}

#[test]
fn nested_macro_blocks_use_the_normal_attribute_policy() {
    for attribute in [
        "#[allow(warnings)]",
        "#[cfg_attr(unix, allow(warnings))]",
        "#[cfg_attr(unix, cfg_attr(test, expect(clippy::all)))]",
    ] {
        let source = format!("fn run() {{ let _ = vec![{{ {attribute} fn inner() {{}} 1 }}]; }}");
        let errors = suppression::check("src/a.rs", &source).unwrap();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("forbidden lint suppression")),
            "{errors:?}"
        );
        assert!(
            scanned(&source)
                .unwrap_err()
                .contains("forbidden lint suppression")
        );
    }
    assert!(
        findings("fn run() { let _ = vec![{ #[deny(warnings)] fn inner() {} 1 }]; }").is_empty()
    );
}

#[test]
fn nested_unknown_item_macros_and_source_attributes_are_not_opaque() {
    for body in [
        "custom!()",
        "vec![custom!()]",
        r#"include!("hidden.rs")"#,
        "{ macro_rules! hidden { () => { fn hidden() {} } } 1 }",
        r#"{ #[path = "hidden.rs"] mod hidden; 1 }"#,
        r#"{ #[cfg_attr(unix, path = "hidden.rs")] mod hidden; 1 }"#,
    ] {
        let source = format!("fn run() {{ let _ = vec![{body}]; }}");
        assert!(scanned(&source).is_err(), "{body}");
    }
}

#[test]
fn xtask_quote_inputs_are_token_data_not_executable_source() {
    let source = r#"fn tokens() {
        let _ = ::quote::quote!(fn example() { #[allow(warnings)] custom!(crate::b::B); });
        let _ = ::quote::quote!(#sig #block #( #items ),*);
        let _ = Token![,];
        let _ = format!("quote!({})", vec![{ if ready() { 1 } else { 0 } }][0]);
    }"#;
    let root = tempdir().unwrap();
    fs::create_dir_all(root.path().join("xtask/src")).unwrap();
    fs::write(root.path().join("xtask/src/lib.rs"), source).unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"xtask\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(root.path().join("xtask/Cargo.toml"),
        "[package]\nname = \"xtask\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nquote = \"1\"\n").unwrap();
    assert!(
        std::process::Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success()
    );
    let scan = scan::scan(root.path(), &["xtask/src"]).unwrap();
    assert!(scan.feedback.is_empty());
    assert!(scan.suppressions.is_empty());
    assert!(ledger::validate(&measurements::collect(&scan, Limits::default()), &[]).is_empty());
    assert!(metrics::analyze("src/a.rs", source).is_err());
}

#[test]
fn macro_traversal_preserves_nesting_and_source_locations() {
    let plain = metrics::analyze(
        "src/a.rs",
        "fn f() { if ready() { if ready() { operation()?; } } }",
    )
    .unwrap();
    let wrapped = metrics::analyze(
        "src/a.rs",
        "fn f() { if ready() { let _ = vec![{ if ready() { operation()?; } }]; } }",
    )
    .unwrap();
    assert_eq!(
        wrapped.functions[0].cyclomatic,
        plain.functions[0].cyclomatic
    );
    assert_eq!(wrapped.functions[0].cognitive, plain.functions[0].cognitive);
    let local = metrics::analyze(
        "src/a.rs",
        "fn f() { let _ = vec![{\nfn local() {}\n1 }]; }",
    )
    .unwrap();
    assert_eq!(
        local
            .functions
            .iter()
            .find(|f| f.symbol.ends_with("::local"))
            .unwrap()
            .line,
        2
    );
}

#[test]
fn existing_repository_macro_forms_remain_scannable() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let result = scan::scan(root, &["src", "tests", "xtask/src", "xtask/tests"]);
    assert!(result.is_ok(), "{}", result.unwrap_err());
}

#[test]
fn macro_limits_remain_inclusive_without_debt_exceptions() {
    let operations = "operation()?; ".repeat(24);
    let methods = (0..20)
        .map(|i| format!("fn m{i}() {{}} "))
        .collect::<String>();
    let source = format!(
        "fn operation() -> Result<(), ()> {{ Ok(()) }}
        fn run() -> Result<(), ()> {{ let _ = vec![{{ {operations}
        struct T; impl T {{ {methods} }} 0 }}]; Ok(()) }}"
    );
    let scan = scanned(&source).unwrap();
    let measured = measurements::collect(&scan, Limits::default());
    assert!(
        measured
            .iter()
            .any(|m| m.key.ends_with("run:cyclomatic") && m.value == 25)
    );
    assert!(
        measured
            .iter()
            .any(|m| m.key.ends_with("T:type_methods") && m.value == 20)
    );
    assert!(ledger::validate(&measured, &[]).is_empty());
}

#[test]
fn executable_format_destinations_json_keys_and_nested_patterns_are_traversed() {
    let block = costly_block();
    for invocation in [
        format!("write!({block}, \"message\")"),
        format!("writeln!({block})"),
        format!("assert!(true, \"{{}}\", {block})"),
        format!("panic!(\"{{value}}\", value = {block})"),
        format!("json!({{ ({block}).to_string(): 1 }})"),
        format!("matches!(0, const {block})"),
    ] {
        let source = format!("fn run() {{ let _ = {invocation}; }}");
        let errors = findings(&source);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("run:cyclomatic: cyclomatic=27")),
            "{invocation}: {errors:?}"
        );
    }
}

#[test]
fn macro_function_and_type_lines_reach_ledger_validation() {
    let statements = "let value = 1;\n".repeat(81);
    let errors = findings(&format!(
        "fn run() {{ let _ = vec![{{\n{statements}0 }}]; }}"
    ));
    assert!(
        errors
            .iter()
            .any(|e| e.contains("run:function_lines") && e.contains("limit=80"))
    );
    let body = "let value = 1;\n".repeat(401);
    let source = format!(
        "fn run() {{ let _ = vec![{{ struct T; impl T {{ fn method() {{\n{body}}} }} 0 }}]; }}"
    );
    let errors = findings(&source);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("T:type_lines") && e.contains("limit=400"))
    );
}
