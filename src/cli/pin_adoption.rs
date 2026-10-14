use crate::{
    config::Config,
    state::{
        StateStore,
        pin_adoption::{self, PinAdoption, PinAdoptionRequest},
    },
};
use serde_json::json;
use std::{fs, path::Path};

const EXPECTED: &str = "expected TASK --config PATH --executable PATH --sha256 HEX --actor LOGIN --reason TEXT [--execute]";

fn arguments(args: &[String]) -> Result<bool, &'static str> {
    if !(args.len() == 11 || args.len() == 12) || args.len() == 12 && args[11] != "--execute" {
        return Err(EXPECTED);
    }
    if !crate::cli_identifiers::valid_attempt(&args[0]) {
        return Err(EXPECTED);
    }
    for (pair, flag) in args[1..11].as_chunks::<2>().0.iter().zip([
        "--config",
        "--executable",
        "--sha256",
        "--actor",
        "--reason",
    ]) {
        if pair[0] != flag || pair[1].trim().is_empty() || pair[1].starts_with('-') {
            return Err(EXPECTED);
        }
    }
    Ok(args.len() == 12)
}

fn apply(args: &[String]) -> Result<(bool, PinAdoption), Box<dyn std::error::Error>> {
    let execute = arguments(args)?;
    let config = Config::from_json(&fs::read_to_string(&args[2])?)?;
    let request = PinAdoptionRequest {
        executable: Path::new(&args[4]),
        sha256: &args[6],
        actor: &args[8],
        reason: &args[10],
    };
    let audit = if execute {
        let mut store = StateStore::open(&config.state_root, config.capacity)?;
        pin_adoption::adopt(&mut store, &args[0], request)?
    } else {
        pin_adoption::preview(&config.state_root, &args[0], request)?
    };
    Ok((execute, audit))
}

/// Exactly one JSON document on stdout, including a refusal; errors also return
/// nonzero through the normal entry point. No worker or GitHub side effects.
pub(crate) fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    match apply(&args) {
        Ok((execute, audit)) => {
            println!(
                "{}",
                json!({"command":"adopt-pin", "task_id":audit.task_id,
                "status":if execute {"adopted"} else {"preview"}, "execute":execute,
                "pin_status":if execute {"pinned"} else {"legacy_unrecorded"},
                "adoption":audit})
            );
            Ok(())
        }
        Err(error) => {
            println!(
                "{}",
                json!({"command":"adopt-pin", "status":"refused", "reason":error.to_string()})
            );
            Err(error)
        }
    }
}
