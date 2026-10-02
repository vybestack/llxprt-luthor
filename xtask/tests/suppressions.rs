use xtask::suppression::check;

#[test]
fn actual_lint_suppressions_including_nested_cfg_attr_are_rejected() {
    for code in [
        "#[allow(clippy::all)] fn a() {}",
        "#![expect(clippy::cognitive_complexity)]",
        "#[cfg_attr(unix, allow(\n clippy::all\n))] fn a() {}",
        "#[cfg_attr(unix, cfg_attr(feature = \"a\", expect(warnings)))] fn a() {}",
    ] {
        let errors = check("src/a.rs", code).unwrap();
        assert!(!errors.is_empty(), "{code}");
        assert!(errors[0].contains("src/a.rs"));
    }
}

#[test]
fn comments_strings_and_deny_are_not_exceptions() {
    assert!(check("src/a.rs", "// #[allow(clippy::all)]\n#[deny(warnings)] fn a() { let s = \"#[expect(warnings)]\"; }").unwrap().is_empty());
    assert!(check("src/a.rs", "#[cfg_attr(unix, allow(] fn a() {}").is_err());
}
