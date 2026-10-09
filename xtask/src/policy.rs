//! Enforce every measurement's limit without exceptions.
use crate::measurement::Measurement;
use std::collections::BTreeMap;

pub fn validate(measurements: &[Measurement]) -> Vec<String> {
    let mut maxima: BTreeMap<&str, &Measurement> = BTreeMap::new();
    for measured in measurements.iter().filter(|m| m.value > m.limit) {
        let previous = maxima.entry(&measured.key).or_insert(measured);
        if measured.value > previous.value {
            *previous = measured;
        }
    }
    maxima.values().map(|m| m.diagnostic()).collect()
}
