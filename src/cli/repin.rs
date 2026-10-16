use crate::{
    config::Config,
    state::{
        StateStore,
        pin_replacement::{self, PinReplacement, PinReplacementRequest},
    },
};
use serde_json::json;
use std::{fs, path::Path};

fn apply(args: &[String]) -> Result<(bool, PinReplacement), Box<dyn std::error::Error>> {
    let execute = crate::cli::pin_adoption::arguments(args)?;
    let config = Config::from_json(&fs::read_to_string(&args[2])?)?;
    let request = PinReplacementRequest {
        executable: Path::new(&args[4]),
        sha256: &args[6],
        actor: &args[8],
        reason: &args[10],
    };
    let audit = if execute {
        let mut store = StateStore::open(&config.state_root, config.capacity)?;
        pin_replacement::replace(&mut store, &config, &args[0], request)?
    } else {
        pin_replacement::preview(&config, &args[0], request)?
    };
    Ok((execute, audit))
}

pub(crate) fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    match apply(&args) {
        Ok((execute, audit)) => {
            println!(
                "{}",
                json!({
                    "command":"repin", "task_id":audit.task_id,
                    "status":if execute {"replaced"} else {"preview"},
                    "execute":execute, "pin_status":"pinned", "replacement":audit
                })
            );
            Ok(())
        }
        Err(error) => {
            println!(
                "{}",
                json!({"command":"repin", "status":"refused", "reason":error.to_string()})
            );
            Err(error)
        }
    }
}
