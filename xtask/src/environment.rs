//! Refuse external settings that can substitute or suppress the repository policy.
use std::{collections::BTreeMap, path::Path};

pub fn validate(values: &BTreeMap<String, String>) -> Result<(), String> {
    for key in [
        "CLIPPY_CONF_DIR",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        if values.contains_key(key) {
            return Err(format!("external policy override forbidden: {key}"));
        }
    }
    if let Some(home) = values.get("CARGO_HOME") {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("missing workspace")?;
        if !Path::new(home).starts_with(root.join("tmp")) {
            return Err("CARGO_HOME must be confined under workspace tmp".into());
        }
        for filename in ["config", "config.toml"] {
            if Path::new(home).join(filename).exists() {
                return Err(format!("external Cargo config forbidden: {filename}"));
            }
        }
    }
    Ok(())
}

pub fn clippy_policy(text: &str) -> Result<(), String> {
    let mut entries = BTreeMap::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
    {
        let (key, value) = line.split_once('=').ok_or("malformed root Clippy policy")?;
        if entries.insert(key.trim(), value.trim()).is_some() {
            return Err("duplicate root Clippy setting".into());
        }
    }
    if entries
        != BTreeMap::from([
            ("cognitive-complexity-threshold", "30"),
            ("type-complexity-threshold", "250"),
        ])
    {
        return Err("root Clippy policy must enforce cognitive 30 and type complexity 250".into());
    }
    Ok(())
}
