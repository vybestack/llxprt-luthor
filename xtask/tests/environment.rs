use std::collections::BTreeMap;
use xtask::environment::validate;

#[test]
fn external_clippy_and_compiler_overrides_are_rejected() {
    assert!(validate(&BTreeMap::new()).is_ok());
    for key in [
        "CLIPPY_CONF_DIR",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_HOME",
    ] {
        let values = BTreeMap::from([(key.into(), "/outside/weaker".into())]);
        assert!(validate(&values).is_err(), "{key}");
    }
}

#[test]
fn malformed_or_weaker_root_clippy_policy_fails_closed() {
    use xtask::environment::clippy_policy;
    assert!(
        clippy_policy("cognitive-complexity-threshold = 30\ntype-complexity-threshold = 250\n")
            .is_ok()
    );
    for text in [
        "",
        "cognitive-complexity-threshold = 300\ntype-complexity-threshold = 250",
        "cognitive-complexity-threshold = 30\ntype-complexity-threshold = 250\nunknown = 1",
        "cognitive-complexity-threshold = 30\ntype-complexity-threshold = 250\ncognitive-complexity-threshold = 99",
    ] {
        assert!(clippy_policy(text).is_err());
    }
}
