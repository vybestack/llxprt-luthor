use serde_json::{Value, json};
use std::{fs, path::Path};

fn fixture() -> (tempfile::TempDir, Value) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for path in [
        "src/lib.rs",
        "tests/basic.rs",
        "xtask/src/main.rs",
        "xtask/tests/basic.rs",
    ] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "fn main() {}\n").unwrap();
    }
    fs::write(root.join("Cargo.toml"), "").unwrap();
    fs::write(root.join("xtask/Cargo.toml"), "").unwrap();
    let package = |name: &str, manifest: &str, targets: Vec<Value>| {
        json!({
            "name": name, "id": format!("{name} 0.1.0 (path+file:///{name})"),
            "manifest_path": root.join(manifest), "targets": targets
        })
    };
    let target =
        |src: &str, kind: &str| json!({"src_path": root.join(src), "kind": [kind], "test": true});
    let packages = vec![
        package(
            "luthor",
            "Cargo.toml",
            vec![
                target("src/lib.rs", "lib"),
                target("tests/basic.rs", "test"),
            ],
        ),
        package(
            "xtask",
            "xtask/Cargo.toml",
            vec![
                target("xtask/src/main.rs", "bin"),
                target("xtask/tests/basic.rs", "test"),
            ],
        ),
    ];
    let members: Vec<_> = packages.iter().map(|p| p["id"].clone()).collect();
    (
        dir,
        json!({"workspace_root": root, "workspace_members": members, "packages": packages}),
    )
}

fn target(metadata: &mut Value, package: usize, src: &str, kind: &str) {
    let root = Path::new(metadata["workspace_root"].as_str().unwrap());
    let path = root.join(src);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "#[allow(dead_code)]\nfn suppressed() {}\n").unwrap();
    metadata["packages"][package]["targets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"src_path": path, "kind": [kind], "test": true}));
}

#[test]
fn ordinary_workspace_inventory_is_accepted() {
    let (_dir, metadata) = fixture();
    xtask::inventory::validate_metadata(
        Path::new(metadata["workspace_root"].as_str().unwrap()),
        &metadata,
    )
    .unwrap();
}

#[test]
fn rejects_unscanned_cargo_target_kinds_and_paths() {
    for (kind, path) in [
        ("custom-build", "build.rs"),
        ("example", "examples/demo.rs"),
        ("bench", "benches/perf.rs"),
        ("bin", "alternate/main.rs"),
    ] {
        let (dir, mut metadata) = fixture();
        target(&mut metadata, 0, path, kind);
        let source = fs::read_to_string(dir.path().join(path)).unwrap();
        assert!(!xtask::suppression::check(path, &source).unwrap().is_empty());
        let error = xtask::inventory::validate_metadata(dir.path(), &metadata).unwrap_err();
        assert!(
            error.contains("unsupported compiled target kind")
                || error.contains("outside scanned roots"),
            "{kind}: {error}"
        );
    }
}

#[test]
fn rejects_additional_workspace_member() {
    let (dir, mut metadata) = fixture();
    let id = "extra 0.1.0 (path+file:///extra)";
    metadata["workspace_members"]
        .as_array_mut()
        .unwrap()
        .push(json!(id));
    metadata["packages"].as_array_mut().unwrap().push(json!({"name":"extra", "id":id, "manifest_path":dir.path().join("extra/Cargo.toml"), "targets":[{"src_path":dir.path().join("extra/src/lib.rs"),"kind":["lib"],"test":true}]}));
    assert!(
        xtask::inventory::validate_metadata(dir.path(), &metadata)
            .unwrap_err()
            .contains("workspace member inventory")
    );
}

#[test]
fn rejects_non_rs_sources_for_every_supported_target_kind_and_root() {
    for (package, prefix) in [
        (0, "src"),
        (0, "tests"),
        (1, "xtask/src"),
        (1, "xtask/tests"),
    ] {
        for kind in ["lib", "bin", "test"] {
            for filename in [
                "hidden.txt",
                "hidden",
                "hidden.RS",
                "hidden.rs.txt",
                "hidden.rs/entry",
            ] {
                let (dir, mut metadata) = fixture();
                target(
                    &mut metadata,
                    package,
                    &format!("{prefix}/{filename}"),
                    kind,
                );
                let error = xtask::inventory::validate_metadata(dir.path(), &metadata).unwrap_err();
                assert!(
                    error.contains("unsupported compiled target source extension"),
                    "{kind}: {error}"
                );
            }
        }
    }
}

#[test]
fn nested_rs_sources_and_rs_final_extensions_remain_supported() {
    let (dir, mut metadata) = fixture();
    target(&mut metadata, 0, "src/nested/hidden.txt.rs", "bin");
    target(&mut metadata, 1, "xtask/tests/nested/entry.rs", "test");
    xtask::inventory::validate_metadata(dir.path(), &metadata).unwrap();
}

#[test]
fn target_path_tricks_cannot_enter_the_scanned_inventory() {
    for source in [
        "src/../hidden.rs",
        "src-extra/hidden.rs",
        "src\\hidden.rs",
        "xtask/src\\hidden.rs",
    ] {
        let (dir, mut metadata) = fixture();
        target(&mut metadata, 0, source, "bin");
        let error = xtask::inventory::validate_metadata(dir.path(), &metadata).unwrap_err();
        assert!(
            error.contains("unsupported target source path")
                || error.contains("outside scanned roots"),
            "{source}: {error}"
        );
    }
}

#[cfg(unix)]
#[test]
fn rs_symlink_names_cannot_disguise_non_rs_sources_or_directories() {
    use std::os::unix::fs::symlink;
    for link_directory in [false, true] {
        let (dir, mut metadata) = fixture();
        target(&mut metadata, 0, "src/hidden.txt", "bin");
        let root = Path::new(metadata["workspace_root"].as_str().unwrap());
        let source = if link_directory {
            fs::create_dir(root.join("outside")).unwrap();
            fs::write(root.join("outside/hidden.rs"), "fn main() {}\n").unwrap();
            symlink(root.join("outside"), root.join("src/linked")).unwrap();
            root.join("src/linked/hidden.rs")
        } else {
            symlink(root.join("src/hidden.txt"), root.join("src/hidden.rs")).unwrap();
            root.join("src/hidden.rs")
        };
        let targets = metadata["packages"][0]["targets"].as_array_mut().unwrap();
        targets.last_mut().unwrap()["src_path"] = json!(source);
        let error = xtask::inventory::validate_metadata(dir.path(), &metadata).unwrap_err();
        assert!(error.contains("target source symlink forbidden"), "{error}");
    }
}
