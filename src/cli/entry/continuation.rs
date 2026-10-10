use crate::{
    config::Config,
    coordinator::{
        ContinuationDependencies, ContinuationResult, OsContinuationLocalInspector,
        OsContinuationProcessInspector, ProductionLauncher, continue_never_dispatched,
    },
    github::{project::GhProjectReader, pull_request::GhPullRequestReader},
    state::StateStore,
};
use serde_json::json;
use std::{fs, path::PathBuf};

pub(crate) fn continue_undispatched(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() != 13
        || args[0] != "--task"
        || args[2] != "--attempt"
        || args[4] != "--config"
        || args[6] != "--config-revision"
        || args[8] != "--actor"
        || args[10] != "--reason"
        || args[11] != "legacy_preflight_recovery"
        || args[12] != "--execute"
        || [1, 3, 5, 7, 9, 11]
            .iter()
            .any(|i| args[*i].trim().is_empty() || args[*i].starts_with('-'))
    {
        return Err("expected --task TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason legacy_preflight_recovery --execute".into());
    }
    // Current template policy may reject a historical plan. This path compares
    // the complete config and validates the saved initial plan without changing it.
    let config: Config = serde_json::from_str(
        &fs::read_to_string(&args[5]).map_err(|_| "continuation configuration unavailable")?,
    )
    .map_err(|_| "continuation configuration invalid")?;
    let mut store = StateStore::open(&config.state_root, config.capacity)
        .map_err(|_| "continuation state unavailable")?;
    let mut projects = GhProjectReader::new(PathBuf::from("gh"));
    let mut prs = GhPullRequestReader::new(PathBuf::from("gh"));
    let mut launcher = ProductionLauncher;
    let mut local = OsContinuationLocalInspector;
    let mut processes = OsContinuationProcessInspector;
    let result = continue_never_dispatched(
        &mut store,
        ContinuationDependencies {
            task_id: &args[1],
            attempt_id: &args[3],
            config: &config,
            config_revision: &args[7],
            actor: &args[9],
            projects: &mut projects,
            prs: &mut prs,
            launcher: &mut launcher,
            local: &mut local,
            processes: &mut processes,
        },
    )
    .map_err(|_| "continuation could not be completed")?;
    match result {
        ContinuationResult::Dispatched(plan) => println!(
            "{}",
            json!({"task_id":plan.task_id,"attempt_id":plan.attempt_id,
                "config_revision":plan.config_revision,"status":"dispatched"})
        ),
        ContinuationResult::Held(reason) => {
            println!(
                "{}",
                json!({"task_id":args[1],"attempt_id":args[3],"status":"held","reason":reason})
            );
            return Err("continuation refused or held".into());
        }
    }
    Ok(())
}
