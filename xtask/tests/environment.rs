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

#[test]
fn cargo_encoded_build_and_target_overrides_cannot_substitute_weaker_compiler_or_config() {
    for key in [
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
        "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS",
        "CARGO_ALIAS_XTASK",
        "CARGO_HOME",
    ] {
        let value = if key == "CARGO_HOME" {
            format!(
                "{}/tmp/../outside",
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .display()
            )
        } else {
            "-A warnings".into()
        };
        let error = validate(&BTreeMap::from([(key.into(), value)])).unwrap_err();
        assert!(
            error.contains("override") || error.contains("confined"),
            "{error}"
        );
    }
}
