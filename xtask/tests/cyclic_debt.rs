use std::{collections::BTreeMap, fs};
use xtask::{
    ledger::{Entry, Measurement, validate},
    measurements::collect,
    metrics::Limits,
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

fn owned(keys: &[&str]) -> Vec<Entry> {
    keys.iter()
        .map(|key| Entry {
            key: (*key).into(),
            ceiling: 1,
            owner: "https://github.com/vybestack/llxprt-luthor/issues/6".into(),
        })
        .collect()
}

fn baseline() -> Vec<Entry> {
    owned(&[
        "coupling::src/a->src/b:cyclic_edge",
        "coupling::src/b->src/a:feedback",
    ])
}

fn measurements(scan: &Scan) -> Vec<Measurement> {
    collect(scan, Limits::default())
}

#[test]
fn exact_owned_baseline_accounts_for_every_cyclic_edge_once() {
    let scan = fixture(&[("a", &["b"]), ("b", &["a"]), ("c", &[])]);
    let measured = measurements(&scan);
    assert!(validate(&measured, &baseline()).is_empty());
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
fn extended_path_behind_owned_feedback_requires_two_new_exact_entries() {
    let original = fixture(&[("a", &["b"]), ("b", &["a"]), ("c", &[])]);
    let extended = fixture(&[("a", &["b", "c"]), ("b", &["a"]), ("c", &["b"])]);
    assert_eq!(original.feedback, extended.feedback);
    let measured = measurements(&extended);
    let findings = validate(&measured, &baseline());
    assert_eq!(findings.len(), 2, "{findings:?}");
    for edge in ["src/a->src/c", "src/c->src/b"] {
        assert!(
            findings.iter().any(|f| f.contains(&format!(
                "coupling::{edge}:cyclic_edge: cyclic_edge=1 limit=0"
            ))),
            "{findings:?}"
        );
    }
    let mut updated = baseline();
    updated.extend(owned(&[
        "coupling::src/a->src/c:cyclic_edge",
        "coupling::src/c->src/b:cyclic_edge",
    ]));
    assert!(validate(&measured, &updated).is_empty());
}

#[test]
fn connecting_cycles_accounts_for_the_previously_acyclic_bridge_too() {
    let original = fixture(&[
        ("a", &["b"]),
        ("b", &["a", "c"]),
        ("c", &["d"]),
        ("d", &["c"]),
    ]);
    let entries = owned(&[
        "coupling::src/a->src/b:cyclic_edge",
        "coupling::src/b->src/a:feedback",
        "coupling::src/c->src/d:cyclic_edge",
        "coupling::src/d->src/c:feedback",
    ]);
    assert!(validate(&measurements(&original), &entries).is_empty());
    let connected = fixture(&[
        ("a", &["b"]),
        ("b", &["a", "c"]),
        ("c", &["a", "d"]),
        ("d", &["c"]),
    ]);
    let findings = validate(&measurements(&connected), &entries);
    assert_eq!(findings.len(), 2, "{findings:?}");
    assert!(
        findings
            .iter()
            .any(|f| f.starts_with("coupling::src/b->src/c:cyclic_edge: cyclic_edge=1 limit=0")),
        "{findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|f| f.starts_with("coupling::src/c->src/a:feedback: feedback=1 limit=0")),
        "{findings:?}"
    );
}

#[test]
fn removed_or_no_longer_cyclic_edges_require_explicit_debt_removal() {
    for dependencies in [
        vec![("a", &[][..]), ("b", &["a"][..])],
        vec![("a", &["b"][..]), ("b", &[][..])],
    ] {
        let measured = measurements(&fixture(&dependencies));
        let findings = validate(&measured, &baseline());
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(
            findings
                .iter()
                .all(|f| f.ends_with(": stale debt symbol/edge"))
        );
        assert!(validate(&measured, &[]).is_empty());
    }
}

#[test]
fn same_size_replacement_fails_with_new_and_stale_keys() {
    let replaced = fixture(&[("a", &["c"]), ("b", &[]), ("c", &["a"])]);
    let findings = validate(&measurements(&replaced), &baseline());
    assert_eq!(findings.len(), 4, "{findings:?}");
    for key in ["src/a->src/b:cyclic_edge", "src/b->src/a:feedback"] {
        assert!(findings.contains(&format!("coupling::{key}: stale debt symbol/edge")));
    }
    for key in ["src/a->src/c:cyclic_edge", "src/c->src/a:feedback"] {
        assert!(
            findings
                .iter()
                .any(|f| f.starts_with(&format!("coupling::{key}:")))
        );
    }
}

#[test]
fn acyclic_growth_does_not_create_cycle_debt() {
    let scan = fixture(&[("a", &["b", "c"]), ("b", &["a"]), ("c", &["d"]), ("d", &[])]);
    assert!(validate(&measurements(&scan), &baseline()).is_empty());
}
