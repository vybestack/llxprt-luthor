use std::{collections::BTreeMap, fs};
use xtask::{
    measurement::Measurement,
    measurements::collect,
    metrics::Limits,
    policy::validate,
    scan::{Scan, scan},
};

fn fixture(dependencies: &[(&str, &[&str])]) -> Scan {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    let mut lib = String::new();
    for (module, targets) in dependencies {
        lib.push_str(&format!("mod {module};\n"));
        let calls: String = targets
            .iter()
            .map(|target| format!("crate::{target}::f();"))
            .collect();
        fs::write(
            root.path().join(format!("src/{module}.rs")),
            format!("pub fn f() {{ {calls} }}"),
        )
        .unwrap();
    }
    fs::write(root.path().join("src/lib.rs"), lib).unwrap();
    scan(root.path(), &["src"]).unwrap()
}

fn measurements(scan: &Scan) -> Vec<Measurement> {
    collect(scan, Limits::default())
}

fn assert_edges(findings: &[String], keys: &[&str]) {
    assert_eq!(findings.len(), keys.len(), "{findings:?}");
    for key in keys {
        let metric = key.rsplit(':').next().unwrap();
        assert!(
            findings.contains(&format!("coupling::{key}: {metric}=1 limit=0")),
            "{findings:?}"
        );
    }
}

#[test]
fn every_cyclic_edge_is_measured_once_and_rejected_including_feedback() {
    let scan = fixture(&[("a", &["b"]), ("b", &["a"]), ("c", &[])]);
    let measured = measurements(&scan);
    assert_edges(
        &validate(&measured),
        &["src/a->src/b:cyclic_edge", "src/b->src/a:feedback"],
    );
    let cyclic: BTreeMap<_, _> = measured
        .iter()
        .filter(|m| m.key.starts_with("coupling::"))
        .map(|m| (m.key.as_str(), (m.value, m.limit)))
        .collect();
    assert_eq!(
        cyclic,
        BTreeMap::from([
            ("coupling::src/a->src/b:cyclic_edge", (1, 0)),
            ("coupling::src/b->src/a:feedback", (1, 0)),
        ])
    );
    assert!(measured.windows(2).all(|p| p[0].key < p[1].key));
}

#[test]
fn extended_path_behind_feedback_rejects_original_and_new_cyclic_edges() {
    let original = fixture(&[("a", &["b"]), ("b", &["a"]), ("c", &[])]);
    let extended = fixture(&[("a", &["b", "c"]), ("b", &["a"]), ("c", &["b"])]);
    assert_eq!(original.feedback, extended.feedback);
    assert_edges(
        &validate(&measurements(&extended)),
        &[
            "src/a->src/b:cyclic_edge",
            "src/b->src/a:feedback",
            "src/a->src/c:cyclic_edge",
            "src/c->src/b:cyclic_edge",
        ],
    );
}

#[test]
fn connecting_cycles_rejects_the_previously_acyclic_bridge_too() {
    let original = fixture(&[
        ("a", &["b"]),
        ("b", &["a", "c"]),
        ("c", &["d"]),
        ("d", &["c"]),
    ]);
    assert_edges(
        &validate(&measurements(&original)),
        &[
            "src/a->src/b:cyclic_edge",
            "src/b->src/a:feedback",
            "src/c->src/d:cyclic_edge",
            "src/d->src/c:feedback",
        ],
    );
    let connected = fixture(&[
        ("a", &["b"]),
        ("b", &["a", "c"]),
        ("c", &["a", "d"]),
        ("d", &["c"]),
    ]);
    assert_edges(
        &validate(&measurements(&connected)),
        &[
            "src/a->src/b:cyclic_edge",
            "src/b->src/a:feedback",
            "src/c->src/d:cyclic_edge",
            "src/d->src/c:feedback",
            "src/b->src/c:cyclic_edge",
            "src/c->src/a:feedback",
        ],
    );
}

#[test]
fn removed_edges_and_broken_return_paths_pass_without_registry_changes() {
    for dependencies in [
        vec![("a", &[][..]), ("b", &["a"][..])],
        vec![("a", &["b"][..]), ("b", &[][..])],
    ] {
        let scan = fixture(&dependencies);
        assert!(scan.cyclic.is_empty());
        assert!(scan.feedback.is_empty());
        assert!(validate(&measurements(&scan)).is_empty());
    }
}

#[test]
fn same_size_replacement_still_rejects_both_new_cycle_classes() {
    let replaced = fixture(&[("a", &["c"]), ("b", &[]), ("c", &["a"])]);
    assert_edges(
        &validate(&measurements(&replaced)),
        &["src/a->src/c:cyclic_edge", "src/c->src/a:feedback"],
    );
}

#[test]
fn acyclic_growth_creates_no_findings_but_does_not_exempt_existing_cycles() {
    let scan = fixture(&[("a", &["b", "c"]), ("b", &["a"]), ("c", &["d"]), ("d", &[])]);
    assert_edges(
        &validate(&measurements(&scan)),
        &["src/a->src/b:cyclic_edge", "src/b->src/a:feedback"],
    );
    let acyclic = fixture(&[("a", &["b", "c"]), ("b", &[]), ("c", &["d"]), ("d", &[])]);
    assert!(validate(&measurements(&acyclic)).is_empty());
}
