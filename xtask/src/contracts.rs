use crate::driver::{Outcome, Runner, cargo};
use serde::Deserialize;
use std::{collections::BTreeSet, path::Path};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub scenario: String,
    pub target: String,
    pub test: String,
}

pub fn manifest(source: &str) -> Result<Vec<Contract>, String> {
    let contracts: Vec<Contract> =
        serde_json::from_str(source).map_err(|e| format!("contract manifest: {e}"))?;
    if contracts.is_empty() {
        return Err("contract manifest is empty".into());
    }
    let mut identities = BTreeSet::new();
    for contract in &contracts {
        if contract.scenario.is_empty()
            || !identifier(&contract.target)
            || !identifier(&contract.test)
            || !identities.insert((&contract.target, &contract.test))
        {
            return Err(format!("invalid/duplicate contract: {}", contract.scenario));
        }
    }
    Ok(contracts)
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

pub fn verify(contract: &Contract, result: &Outcome) -> Result<(), String> {
    let passed = format!("test {} ... ok", contract.test);
    let summaries: Vec<_> = result
        .stdout
        .lines()
        .filter(|l| l.starts_with("test result:"))
        .collect();
    if result.code == 0
        && result.stdout.lines().filter(|l| *l == passed).count() == 1
        && summaries.len() == 1
        && summaries[0].starts_with("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;")
    {
        Ok(())
    } else {
        Err(format!(
            "contract '{}': missing/ignored/filtered/skipped/failed result for {}::{} (exit {})",
            contract.scenario, contract.target, contract.test, result.code
        ))
    }
}

pub fn run(root: &Path, contracts: &[Contract], runner: &mut impl Runner) -> Result<i32, String> {
    let mut failures = Vec::new();
    let mut exit = 0;
    for contract in contracts {
        let command = cargo(
            root,
            &[
                "test",
                "-p",
                "luthor",
                "--test",
                &contract.target,
                "--all-features",
                "--offline",
                "--locked",
                &contract.test,
                "--",
                "--exact",
                "--test-threads=1",
            ],
        );
        let outcome = runner.run(&command)?;
        if let Err(error) = verify(contract, &outcome) {
            failures.push(error);
            if exit == 0 {
                exit = outcome.code.max(1);
            }
        }
    }
    for error in failures {
        eprintln!("{error}");
    }
    Ok(exit)
}
