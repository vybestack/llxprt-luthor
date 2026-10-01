//! Normalize scan measurements into one deterministic policy inventory.
use crate::{ledger::Measurement, metrics::Limits, scan::Scan};

pub fn collect(scan: &Scan, limits: Limits) -> Vec<Measurement> {
    let mut measurements: Vec<_> = scan
        .reports
        .iter()
        .flat_map(|r| r.measurements(limits))
        .collect();
    measurements.retain(|m| {
        !(m.key.starts_with("src/") || m.key.starts_with("xtask/src/"))
            || (!m.key.ends_with(":type_lines") && !m.key.ends_with(":type_methods"))
    });
    for (key, (lines, methods)) in &scan.types {
        measurements.push(Measurement {
            key: format!("{key}:type_lines"),
            value: *lines,
            limit: limits.type_lines,
        });
        measurements.push(Measurement {
            key: format!("{key}:type_methods"),
            value: *methods,
            limit: limits.type_methods,
        });
    }
    measurements.extend(scan.feedback.iter().map(|(a, b)| Measurement {
        key: format!("coupling::{a}->{b}:feedback"),
        value: 1,
        limit: 0,
    }));
    let mut maxima: std::collections::BTreeMap<String, Measurement> =
        std::collections::BTreeMap::new();
    for m in measurements {
        let previous = maxima.entry(m.key.clone()).or_insert(Measurement {
            key: m.key.clone(),
            value: 0,
            limit: m.limit,
        });
        if m.value > previous.value {
            *previous = m;
        }
    }
    maxima.into_values().collect()
}
