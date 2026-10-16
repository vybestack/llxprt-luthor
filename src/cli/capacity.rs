use crate::{
    config::Config,
    state::capacity::{self, CapacityChange, CapacityChangeRequest},
};
use serde_json::json;
use std::fs;

const EXPECTED: &str = "expected --config PATH --expected-capacity N --expected-reservations N --actor LOGIN --reason TEXT --execute";

fn arguments(args: &[String]) -> Result<(), &'static str> {
    if args.len() != 11 || args[10] != "--execute" {
        return Err(EXPECTED);
    }
    for (pair, flag) in args[..10].as_chunks::<2>().0.iter().zip([
        "--config",
        "--expected-capacity",
        "--expected-reservations",
        "--actor",
        "--reason",
    ]) {
        if pair[0] != flag || pair[1].trim().is_empty() || pair[1].starts_with('-') {
            return Err(EXPECTED);
        }
    }
    Ok(())
}

fn apply(args: &[String]) -> Result<CapacityChange, Box<dyn std::error::Error>> {
    // Explicit authorization and numeric syntax are checked before any I/O.
    arguments(args)?;
    let expected_capacity = args[3].parse()?;
    let expected_reservations = args[5].parse()?;
    let config = Config::from_json(&fs::read_to_string(&args[1])?)?;
    Ok(capacity::change(
        &config.state_root,
        CapacityChangeRequest {
            expected_capacity,
            expected_reservations,
            new_capacity: config.capacity,
            actor: &args[7],
            reason: &args[9],
        },
    )?)
}

pub(crate) fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    match apply(&args) {
        Ok(audit) => {
            println!(
                "{}",
                json!({"command":"resize-capacity", "status":"changed", "change":audit})
            );
            Ok(())
        }
        Err(error) => {
            println!(
                "{}",
                json!({"command":"resize-capacity", "status":"refused", "reason":error.to_string()})
            );
            Err(error)
        }
    }
}
