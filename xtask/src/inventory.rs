use std::{
    path::{Component, Path},
    process::Command,
};

const ROOTS: [&str; 4] = ["src", "tests", "xtask/src", "xtask/tests"];

pub fn scan_roots(root: &Path) -> Result<Vec<&'static str>, String> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--offline",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cargo metadata failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("malformed cargo metadata: {e}"))?;
    validate_metadata(root, &metadata)?;
    Ok(ROOTS.to_vec())
}

pub fn validate_metadata(root: &Path, metadata: &serde_json::Value) -> Result<(), String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("workspace root: {e}"))?;
    let root = canonical_root.as_path();
    let workspace_root = metadata
        .get("workspace_root")
        .and_then(|v| v.as_str())
        .ok_or("cargo metadata missing workspace_root")?;
    if Path::new(workspace_root)
        != root
            .canonicalize()
            .map_err(|e| format!("workspace root: {e}"))?
    {
        return Err("cargo metadata describes a different workspace".into());
    }
    let members = metadata
        .get("workspace_members")
        .and_then(|v| v.as_array())
        .ok_or("cargo metadata missing workspace_members")?;
    let packages = metadata
        .get("packages")
        .and_then(|v| v.as_array())
        .ok_or("cargo metadata missing packages")?;
    let expected = ["luthor", "xtask"];
    if members.len() != expected.len() {
        return Err("unsupported workspace member inventory".into());
    }
    for name in expected {
        let package = packages
            .iter()
            .find(|p| p.get("name").and_then(|n| n.as_str()) == Some(name))
            .ok_or_else(|| format!("missing workspace package {name}"))?;
        validate_package(root, name, package, members)?;
    }
    if packages
        .iter()
        .filter(|p| {
            members
                .iter()
                .any(|m| m.as_str() == p.get("id").and_then(|v| v.as_str()))
        })
        .count()
        != expected.len()
    {
        return Err("unsupported additional workspace member".into());
    }
    Ok(())
}

fn validate_package(
    root: &Path,
    name: &str,
    package: &serde_json::Value,
    members: &[serde_json::Value],
) -> Result<(), String> {
    let id = package
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("package missing id")?;
    if !members.iter().any(|m| m.as_str() == Some(id)) {
        return Err(format!("{name} is not a workspace member"));
    }
    let manifest = package
        .get("manifest_path")
        .and_then(|v| v.as_str())
        .ok_or("package missing manifest_path")?;
    let expected_manifest = if name == "luthor" {
        root.join("Cargo.toml")
    } else {
        root.join("xtask/Cargo.toml")
    };
    if Path::new(manifest) != expected_manifest {
        return Err(format!("unsupported manifest path for {name}"));
    }
    let targets = package
        .get("targets")
        .and_then(|v| v.as_array())
        .ok_or("package missing targets")?;
    if targets.is_empty() {
        return Err(format!("{name}: empty compiled target inventory"));
    }
    for target in targets {
        validate_target(root, name, target)?;
    }
    Ok(())
}

fn validate_target(root: &Path, name: &str, target: &serde_json::Value) -> Result<(), String> {
    let src = target
        .get("src_path")
        .and_then(|v| v.as_str())
        .ok_or("target missing src_path")?;
    let kind = target
        .get("kind")
        .and_then(|v| v.as_array())
        .ok_or("target missing kind")?;
    if kind.len() != 1 || !["lib", "bin", "test"].contains(&kind[0].as_str().unwrap_or("")) {
        return Err(format!(
            "unsupported compiled target kind for {name}: {kind:?}"
        ));
    }
    validate_target_source(root, src)
}

fn validate_target_source(root: &Path, src: &str) -> Result<(), String> {
    let path = Path::new(src);
    if !path.is_absolute() {
        return Err(format!("non-absolute target source: {src}"));
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("target source outside workspace: {src}"))?;
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!("unsupported target source path: {src}"));
    }
    let text = relative.to_string_lossy().replace('\\', "/");
    if !ROOTS.iter().any(|prefix| relative.starts_with(prefix)) {
        return Err(format!(
            "compiled target source outside scanned roots: {text}"
        ));
    }
    let mut cursor = root.to_path_buf();
    for component in relative.components() {
        cursor.push(component);
        let info = std::fs::symlink_metadata(&cursor)
            .map_err(|e| format!("target source unavailable {}: {e}", cursor.display()))?;
        if info.file_type().is_symlink() {
            return Err(format!(
                "target source symlink forbidden: {}",
                cursor.display()
            ));
        }
    }
    if !path.is_file() {
        return Err(format!("target source is not a file: {src}"));
    }
    if !path.extension().is_some_and(|extension| extension == "rs") {
        return Err(format!(
            "unsupported compiled target source extension (expected .rs): {src}"
        ));
    }
    Ok(())
}
