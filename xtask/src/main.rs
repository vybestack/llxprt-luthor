use std::{fs, path::PathBuf};
use xtask::{
    driver::Processes,
    ledger::{self, Entry},
    metrics::Limits,
    scan,
};

fn execute() -> Result<i32, String> {
    xtask::environment::validate(&std::env::vars().collect())?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing workspace parent")?
        .to_path_buf();
    let mode = std::env::args()
        .nth(1)
        .ok_or("usage: cargo xtask ci|measure|policy|contracts")?;
    xtask::environment::clippy_policy(
        &fs::read_to_string(root.join("clippy.toml")).map_err(|e| e.to_string())?,
    )?;
    let scan = scan::scan(&root, &["src", "tests", "xtask/src", "xtask/tests"])?;
    let measurements = xtask::measurements::collect(&scan, Limits::default());
    if mode == "measure" {
        println!(
            "{}",
            serde_json::to_string_pretty(&measurements).map_err(|e| e.to_string())?
        );
        return Ok(0);
    }
    let entries: Vec<Entry> = serde_json::from_str(
        &fs::read_to_string(root.join("xtask/debt.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("malformed debt ledger: {e}"))?;
    xtask::ledger::validate_owners(
        &entries,
        &fs::read_to_string(root.join("xtask/owners.json")).map_err(|e| e.to_string())?,
    )?;
    let mut findings = ledger::validate(&measurements, &entries);
    findings.extend(scan.suppressions);
    findings.sort();
    for finding in &findings {
        eprintln!("{finding}");
    }
    if !findings.is_empty() {
        return Ok(1);
    }
    if mode == "policy" {
        return Ok(0);
    }
    let contracts = xtask::contracts::manifest(
        &fs::read_to_string(root.join("xtask/contracts.json")).map_err(|e| e.to_string())?,
    )?;
    let mut processes = Processes;
    if mode == "ci" {
        return xtask::ci::run(&root, &contracts, &mut processes);
    } else if mode != "contracts" {
        return Err(format!("unknown mode: {mode}"));
    }
    xtask::contracts::run(&root, &contracts, &mut processes)
}

fn main() {
    let code = match execute() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("gate: {error}");
            1
        }
    };
    std::process::exit(code);
}
