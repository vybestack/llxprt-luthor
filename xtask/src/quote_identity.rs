//! Verify the extern-prelude identity before treating quote inputs as token data.
use std::{path::Path, process::Command};

pub(crate) fn verify(root: &Path) -> Result<(), String> {
    let output = Command::new("cargo")
        .args(["metadata", "--offline", "--locked", "--format-version", "1"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("quote macro identity: cargo metadata failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "quote macro identity: cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("quote macro identity: malformed cargo metadata: {e}"))?;
    resolved(root, &metadata).map_err(|e| format!("quote macro identity: {e}"))
}

fn resolved(root: &Path, metadata: &serde_json::Value) -> Result<(), String> {
    let packages = metadata["packages"].as_array().ok_or("missing packages")?;
    let manifest = root
        .join("xtask/Cargo.toml")
        .canonicalize()
        .map_err(|e| format!("xtask manifest: {e}"))?;
    let tooling = packages
        .iter()
        .find(|p| p["manifest_path"].as_str() == manifest.to_str())
        .ok_or("missing xtask package")?;
    if !["2018", "2021", "2024"].contains(&tooling["edition"].as_str().unwrap_or("")) {
        return Err("quote token data requires the extern-prelude edition rules".into());
    }
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("missing resolve nodes")?;
    let tooling_node = nodes
        .iter()
        .find(|n| n["id"] == tooling["id"])
        .ok_or("missing xtask resolve node")?;
    let deps = tooling_node["deps"]
        .as_array()
        .ok_or("missing xtask dependencies")?;
    let binding = deps
        .iter()
        .find(|d| d["name"] == "quote")
        .ok_or("missing direct quote dependency")?;
    let dependency = packages
        .iter()
        .find(|p| p["id"] == binding["pkg"])
        .ok_or("missing resolved quote package")?;
    if dependency["name"] != "quote"
        || dependency["source"] != "registry+https://github.com/rust-lang/crates.io-index"
        || !dependency["version"]
            .as_str()
            .is_some_and(|v| v.starts_with("1."))
    {
        return Err(
            "quote must resolve to the registry quote 1.x package, not an alias or replacement"
                .into(),
        );
    }
    Ok(())
}
