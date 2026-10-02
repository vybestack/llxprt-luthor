use xtask::metrics::{Limits, analyze};

fn findings(code: &str, limits: Limits) -> Vec<String> {
    analyze("src/nested/example.rs", code)
        .unwrap()
        .violations(limits)
}

#[test]
fn effective_lines_ignore_comments_not_strings_and_handle_line_endings() {
    for input in ["", "// fn fake() {}\r\n/* code */", "\n"] {
        assert_eq!(analyze("src/a.rs", input).unwrap().file_lines, 0);
    }
    for input in ["fn a() {}", "fn a() {}\r\n", "// comment\nfn a() {}\n"] {
        assert_eq!(analyze("src/a.rs", input).unwrap().file_lines, 1);
    }
    assert_eq!(
        analyze("src/a.rs", "fn a() {\n let s = r#\"\ntext\n\"#;\n}")
            .unwrap()
            .file_lines,
        5
    );
    assert!(analyze("src/a.rs", "fn broken(").is_err());
}

#[test]
fn file_and_function_boundaries_and_cfg_variants() {
    let source = "#[cfg(unix)]\nfn a() {\nlet x = 1;\n}\n#[cfg(windows)]\nfn b() {}";
    let mut limits = Limits {
        file_lines: 4,
        function_lines: 3,
        ..Limits::default()
    };
    assert!(findings(source, limits).is_empty());
    limits.file_lines = 3;
    limits.function_lines = 2;
    let errors = findings(source, limits);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("file_lines=4") && e.contains("limit=3"))
    );
    assert!(errors.iter().any(|e| e.contains("a")
        && e.contains("function_lines=3")
        && e.contains("src/nested/example.rs")));
}

#[test]
fn split_impls_and_free_function_concentration_cannot_evade_limits() {
    let limits = Limits {
        type_methods: 2,
        type_lines: 2,
        module_lines: 2,
        ..Limits::default()
    };
    let source = "struct T;\nimpl T { fn a() {} }\nimpl T { fn b() {} }\ntrait Q { fn c(&self); }\nimpl Q for T { fn c(&self) {} }\nfn x() {}\nfn y() {}\nfn z() {}";
    let errors = findings(source, limits);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("T") && e.contains("type_methods=3"))
    );
    assert!(errors.iter().any(|e| e.contains("type_lines=3")));
    assert!(errors.iter().any(|e| e.contains("module_lines=3")));
    assert!(findings("struct Data { a: u8, b: u8 }", limits).is_empty());
    assert!(findings("fn x() {}\nfn y() {}", limits).is_empty());
}

#[test]
fn branches_and_nesting_have_separate_complexity_limits() {
    let source = "fn a(x: bool) { if x { if x { } } else { } while x { } }";
    let report = analyze("src/a.rs", source).unwrap();
    let f = &report.functions[0];
    assert_eq!(f.cyclomatic, 4);
    assert_eq!(f.cognitive, 5);
    let mut limits = Limits {
        cyclomatic: 4,
        cognitive: 5,
        ..Limits::default()
    };
    assert!(report.violations(limits).is_empty());
    limits.cyclomatic = 3;
    limits.cognitive = 4;
    assert_eq!(report.violations(limits).len(), 2);
}

#[test]
fn hidden_source_and_unexpanded_items_fail_closed() {
    for source in [
        "include!(\"other.rs\");",
        "macro_rules! hidden { () => { fn huge() {} } }",
        "#[path = \"other.rs\"] mod a;",
        "generate_functions!();",
    ] {
        assert!(analyze("src/a.rs", source).is_err(), "{source}");
    }
}

#[test]
fn generic_impls_share_responsibility_and_trait_default_methods_are_measured() {
    let limits = Limits {
        type_methods: 1,
        ..Limits::default()
    };
    let errors = findings(
        "struct T<X>(X); impl<X> T<X> { fn a() {} } impl T<u8> { fn b() {} }",
        limits,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("T") && e.contains("type_methods=2"))
    );
    assert!(!findings("trait Heavy { fn a() {} fn b() {} }", limits).is_empty());
}

#[test]
fn cfg_attr_paths_and_nested_source_generating_macros_cannot_hide_code() {
    for source in [
        "#[cfg_attr(unix, path = \"hidden.rs\")] mod x;",
        "fn a() { custom!(include!(\"hidden.rs\")); }",
        "fn a() { custom! { #[allow(warnings)] fn b() {} } }",
    ] {
        assert!(analyze("src/a.rs", source).is_err());
    }
}

#[test]
fn accepted_expression_macro_cannot_smuggle_nested_generated_source() {
    for source in [
        "fn a() { println!(\"{}\", custom!(include!(\"hidden.rs\"))); }",
        "fn a() { assert!(true, { #[allow(warnings)] 1 }); }",
    ] {
        assert!(analyze("src/a.rs", source).is_err());
    }
    assert!(
        analyze(
            "src/a.rs",
            "fn a() { println!(\"custom!(#[allow(warnings)])\"); }"
        )
        .is_ok()
    );
}

#[test]
fn concentrated_type_and_free_function_boundary_is_inclusive() {
    let source = "struct T; impl T { fn a() {} } impl T { fn b() {} } fn x() {} fn y() {}";
    let exact = Limits {
        type_methods: 2,
        type_lines: 2,
        module_lines: 2,
        ..Limits::default()
    };
    assert!(findings(source, exact).is_empty());
    assert!(
        !findings(
            source,
            Limits {
                type_methods: 1,
                ..exact
            }
        )
        .is_empty()
    );
    assert!(
        !findings(
            source,
            Limits {
                type_lines: 0,
                ..exact
            }
        )
        .is_empty()
    );
    assert!(
        !findings(
            source,
            Limits {
                module_lines: 0,
                ..exact
            }
        )
        .is_empty()
    );
}
