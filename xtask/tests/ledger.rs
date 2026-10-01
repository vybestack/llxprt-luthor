use xtask::ledger::{Entry, Measurement, validate};

fn measurement(value: usize) -> Measurement {
    Measurement {
        key: "src/a.rs::f:function_lines".into(),
        value,
        limit: 80,
    }
}
fn entry(ceiling: usize) -> Entry {
    Entry {
        key: "src/a.rs::f:function_lines".into(),
        ceiling,
        owner: "https://github.com/vybestack/llxprt-luthor/issues/2".into(),
    }
}

#[test]
fn exact_owned_debt_only_and_improvements_require_ratchet() {
    assert!(validate(&[measurement(90)], &[entry(90)]).is_empty());
    for (actual, debt) in [
        (vec![measurement(91)], vec![entry(90)]),
        (vec![measurement(89)], vec![entry(90)]),
        (vec![measurement(90)], vec![]),
        (vec![], vec![entry(90)]),
    ] {
        assert!(!validate(&actual, &debt).is_empty());
    }
    assert!(validate(&[measurement(89)], &[entry(89)]).is_empty());
    for owner in [
        "",
        "*",
        "https://example.com/issues/2",
        "https://github.com/vybestack/llxprt-luthor/issues/0",
    ] {
        let mut debt = entry(90);
        debt.owner = owner.into();
        assert!(!validate(&[measurement(90)], &[debt]).is_empty());
    }
    let mut broad = entry(90);
    broad.key = "src/*".into();
    assert!(!validate(&[measurement(90)], &[broad]).is_empty());
    assert!(!validate(&[measurement(90)], &[entry(90), entry(90)]).is_empty());
}

#[test]
fn offline_owner_registry_rejects_unknown_closed_wrong_repo_or_unassigned_issues() {
    use xtask::ledger::validate_owners;
    let debt = vec![Entry {
        key: "src/a.rs::x:function_lines".into(),
        ceiling: 100,
        owner: "https://github.com/vybestack/llxprt-luthor/issues/6".into(),
    }];
    for registry in [
        "[]",
        r#"[{"url":"https://github.com/wrong/repo/issues/6","state":"OPEN","assignee":"acoliver"}]"#,
        r#"[{"url":"https://github.com/vybestack/llxprt-luthor/issues/6","state":"CLOSED","assignee":"acoliver"}]"#,
        r#"[{"url":"https://github.com/vybestack/llxprt-luthor/issues/6","state":"OPEN","assignee":""}]"#,
    ] {
        assert!(validate_owners(&debt, registry).is_err());
    }
    assert!(validate_owners(&debt, r#"[{"url":"https://github.com/vybestack/llxprt-luthor/issues/6","state":"OPEN","assignee":"acoliver"}]"#).is_ok());
}

#[test]
fn cfg_variants_of_one_symbol_use_the_maximum_measurement() {
    let measurements = vec![
        Measurement {
            key: "src/a.rs::run:function_lines".into(),
            value: 90,
            limit: 80,
        },
        Measurement {
            key: "src/a.rs::run:function_lines".into(),
            value: 2,
            limit: 80,
        },
    ];
    assert!(!validate(&measurements, &[]).is_empty());
}

#[test]
fn duplicated_cfg_measurements_with_debt_cannot_lower_the_maximum_ceiling() {
    let measurements = vec![
        Measurement {
            key: "src/a.rs::run:function_lines".into(),
            value: 90,
            limit: 80,
        },
        Measurement {
            key: "src/a.rs::run:function_lines".into(),
            value: 2,
            limit: 80,
        },
    ];
    let debt = vec![Entry {
        key: measurements[0].key.clone(),
        ceiling: 90,
        owner: "https://github.com/vybestack/llxprt-luthor/issues/6".into(),
    }];
    assert!(validate(&measurements, &debt).is_empty());
}
