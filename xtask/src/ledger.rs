use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub key: String,
    pub ceiling: usize,
    pub owner: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Measurement {
    pub key: String,
    pub value: usize,
    pub limit: usize,
}

impl Measurement {
    pub fn diagnostic(&self, ceiling: usize) -> String {
        let metric = self.key.rsplit(':').next().unwrap_or("metric");
        format!(
            "{}: {metric}={} limit={ceiling} (new-code limit={})",
            self.key, self.value, self.limit
        )
    }
}

pub fn valid_owner(owner: &str) -> bool {
    owner
        .strip_prefix("https://github.com/vybestack/llxprt-luthor/issues/")
        .and_then(|n| n.parse::<u64>().ok())
        .is_some_and(|n| n > 0)
}

pub fn validate(measurements: &[Measurement], entries: &[Entry]) -> Vec<String> {
    let mut findings = Vec::new();
    let mut debt = BTreeMap::new();
    for entry in entries {
        if entry.key.contains('*')
            || !valid_owner(&entry.owner)
            || debt.insert(&entry.key, entry).is_some()
        {
            findings.push(format!(
                "{}: invalid/broad/duplicate debt or owner issue",
                entry.key
            ));
        }
    }
    let mut used = BTreeSet::new();
    let mut maxima: BTreeMap<&str, &Measurement> = BTreeMap::new();
    for measured in measurements {
        let previous = maxima.entry(&measured.key).or_insert(measured);
        if measured.value > previous.value {
            *previous = measured;
        }
    }
    for measured in maxima.values() {
        match debt.get(&measured.key) {
            Some(entry) => {
                used.insert(&entry.key);
                if measured.value <= measured.limit || measured.value < entry.ceiling {
                    findings.push(format!(
                        "{}: stale ceiling {}; lower/remove debt to measured {}",
                        measured.key, entry.ceiling, measured.value
                    ));
                } else if measured.value > entry.ceiling {
                    findings.push(measured.diagnostic(entry.ceiling));
                }
            }
            None if measured.value > measured.limit => {
                findings.push(measured.diagnostic(measured.limit))
            }
            None => {}
        }
    }
    for key in debt.keys() {
        if !used.contains(key) {
            findings.push(format!("{key}: stale debt symbol/edge"));
        }
    }
    findings.sort();
    findings
}

/// Checked-in, reviewed GitHub evidence permits deterministic offline owner validation.
pub fn validate_owners(entries: &[Entry], registry: &str) -> Result<(), String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Owner {
        url: String,
        state: String,
        assignee: String,
    }
    let owners: Vec<Owner> =
        serde_json::from_str(registry).map_err(|e| format!("owner registry: {e}"))?;
    let mut verified = BTreeSet::new();
    for owner in owners {
        if !valid_owner(&owner.url)
            || owner.state != "OPEN"
            || owner.assignee != "acoliver"
            || !verified.insert(owner.url)
        {
            return Err("invalid or duplicate remediation owner evidence".into());
        }
    }
    for entry in entries {
        if !verified.contains(&entry.owner) {
            return Err(format!(
                "{}: unknown remediation issue {}",
                entry.key, entry.owner
            ));
        }
    }
    Ok(())
}
