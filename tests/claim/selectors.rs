use serde_json::Value;
use std::process::Command;

#[test]
fn required_contract_selectors_are_preserved() {
    let contracts: Vec<Value> =
        serde_json::from_str(include_str!("../../xtask/contracts.json")).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--list")
        .output()
        .unwrap();
    assert!(output.status.success());
    let listing = String::from_utf8(output.stdout).unwrap();
    let required: Vec<_> = contracts
        .iter()
        .filter(|contract| contract["target"] == "claim")
        .collect();
    assert!(!required.is_empty());
    for contract in required {
        let selector = contract["test"].as_str().unwrap();
        let entry = format!("{selector}: test");
        assert_eq!(
            listing.lines().filter(|line| *line == entry).count(),
            1,
            "required exact selector: {selector}"
        );
    }
}
