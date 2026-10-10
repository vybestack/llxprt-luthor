use xtask::{measurement::Measurement, metrics::Limits, policy::validate};

fn measurement(key: &str, value: usize, limit: usize) -> Measurement {
    Measurement {
        key: key.into(),
        value,
        limit,
    }
}

#[test]
fn every_default_limit_is_inclusive_and_every_excess_fails() {
    let limits = Limits::default();
    let defaults = [
        ("file_lines", limits.file_lines, 800),
        ("function_lines", limits.function_lines, 80),
        ("cyclomatic", limits.cyclomatic, 25),
        ("cognitive", limits.cognitive, 30),
        ("type_lines", limits.type_lines, 400),
        ("type_methods", limits.type_methods, 20),
        ("module_lines", limits.module_lines, 600),
        ("cyclic_edge", 0, 0),
        ("feedback", 0, 0),
    ];
    for (metric, limit, expected) in defaults {
        assert_eq!(limit, expected);
        let key = format!("src/a.rs::symbol:{metric}");
        for value in [0, limit] {
            assert!(validate(&[measurement(&key, value, limit)]).is_empty());
        }
        for value in [limit + 1, limit + 10, usize::MAX] {
            assert_eq!(
                validate(&[measurement(&key, value, limit)]),
                [format!("{key}: {metric}={value} limit={limit}")]
            );
        }
    }
    assert!(validate(&[]).is_empty());
}

#[test]
fn no_path_symbol_or_metric_key_is_exempt_from_its_limit() {
    for key in [
        "src/a.rs::f:function_lines",
        "tests/a.rs::f:function_lines",
        "xtask/src/a.rs::f:function_lines",
        "xtask/tests/a.rs::f:function_lines",
        "src/*::f:function_lines",
        "unknown",
    ] {
        assert_eq!(validate(&[measurement(key, 90, 80)]).len(), 1);
    }
}

#[test]
fn cfg_variants_use_the_maximum_in_either_order_and_fail_once() {
    let key = "src/a.rs::run:function_lines";
    for values in [[90, 2, 89], [89, 2, 90]] {
        let measured: Vec<_> = values
            .into_iter()
            .map(|value| measurement(key, value, 80))
            .collect();
        assert_eq!(
            validate(&measured),
            ["src/a.rs::run:function_lines: function_lines=90 limit=80"]
        );
    }
}

#[test]
fn every_over_limit_record_fails_even_when_another_variant_has_a_larger_limit() {
    let key = "src/a.rs::run:function_lines";
    assert_eq!(
        validate(&[measurement(key, 81, 80), measurement(key, 90, 100)]),
        ["src/a.rs::run:function_lines: function_lines=81 limit=80"]
    );
}

#[test]
fn findings_are_sorted_and_under_limit_improvements_need_no_registry() {
    let a = "src/a.rs::run:function_lines";
    let b = "src/b.rs::run:function_lines";
    assert_eq!(
        validate(&[measurement(b, 91, 80), measurement(a, 90, 80)]),
        [
            "src/a.rs::run:function_lines: function_lines=90 limit=80",
            "src/b.rs::run:function_lines: function_lines=91 limit=80",
        ]
    );
    assert!(validate(&[measurement(a, 79, 80), measurement(b, 80, 80)]).is_empty());
}

#[test]
fn serialized_measurements_keep_only_key_value_and_limit() {
    assert_eq!(
        serde_json::to_value(measurement("src/a.rs::f:function_lines", 80, 80)).unwrap(),
        serde_json::json!({"key": "src/a.rs::f:function_lines", "value": 80, "limit": 80})
    );
}
