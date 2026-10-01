use std::collections::{BTreeMap, BTreeSet};
use xtask::{
    measurements::collect,
    metrics::{Limits, Report},
    scan::Scan,
};

#[test]
fn alternatives_take_the_maximum_and_cross_file_types_replace_local_totals() {
    let report = |path: &str, lines, methods| Report {
        path: path.into(),
        file_lines: lines,
        types: BTreeMap::from([("Record".into(), (lines, methods))]),
        ..Report::default()
    };
    let scan = Scan {
        reports: vec![
            report("src/domain.rs", 10, 1),
            report("src/domain.rs", 30, 2),
            report("tests/domain.rs", 40, 3),
        ],
        feedback: BTreeSet::from([("a".into(), "b".into())]),
        suppressions: vec![],
        types: BTreeMap::from([("src::domain::Record".into(), (50, 4))]),
    };
    let actual = collect(&scan, Limits::default());
    let values: BTreeMap<_, _> = actual.iter().map(|m| (m.key.as_str(), m.value)).collect();
    assert_eq!(values["src/domain.rs::file:file_lines"], 30);
    assert_eq!(values["src::domain::Record:type_lines"], 50);
    assert_eq!(values["src::domain::Record:type_methods"], 4);
    assert!(
        !actual
            .iter()
            .any(|m| m.key.starts_with("src/domain.rs") && m.key.ends_with(":type_lines"))
    );
    assert!(actual.iter().any(|m| m.key.starts_with("tests/domain.rs")
        && m.key.ends_with(":type_methods")
        && m.value == 3));
    assert_eq!(values["coupling::a->b:feedback"], 1);
    assert_eq!(
        actual
            .iter()
            .find(|m| m.key == "coupling::a->b:feedback")
            .unwrap()
            .limit,
        0
    );
    assert!(actual.windows(2).all(|pair| pair[0].key < pair[1].key));
}
