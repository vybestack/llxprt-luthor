use crate::{
    cli_identifiers::valid_attempt,
    config::Config,
    coordinator::{
        AmendedContinuationDependencies, ContinuationResult, NativeAmendedSupervisorLauncher,
        OsContinuationLocalInspector, OsContinuationProcessInspector,
        amend_never_dispatched_initial_branch,
    },
    github::{project::GhProjectReader, pull_request::GhPullRequestReader},
    state::StateStore,
};
use serde_json::json;
use std::{fs, path::PathBuf};

const EXPECTED: &str = "expected --task TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason native_initial_branch_is_conversation --execute";

fn arguments(args: &[String]) -> Result<(), &'static str> {
    let flags = [
        "--task",
        "--attempt",
        "--config",
        "--config-revision",
        "--actor",
        "--reason",
    ];
    if args.len() != 13 || args[12] != "--execute" {
        return Err(EXPECTED);
    }
    for (pair, flag) in args[..12].as_chunks::<2>().0.iter().zip(flags) {
        if pair[0] != flag || pair[1].trim().is_empty() || pair[1].starts_with('-') {
            return Err(EXPECTED);
        }
    }
    if args[11] != "native_initial_branch_is_conversation" {
        return Err(EXPECTED);
    }
    for index in [1, 3, 9] {
        if !valid_attempt(&args[index]) {
            return Err(EXPECTED);
        }
    }
    if args[7].len() > 128 || args[7].contains(['\0', '\n', '\r']) {
        return Err(EXPECTED);
    }
    Ok(())
}

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    arguments(&args)?;
    let config = Config::from_json(
        &fs::read_to_string(&args[5]).map_err(|_| "amendment configuration unavailable")?,
    )
    .map_err(|error| error.operator_message("amendment"))?;
    let mut store = StateStore::open(&config.state_root, config.capacity)
        .map_err(|_| "amendment state unavailable")?;
    let mut projects = GhProjectReader::new(PathBuf::from("gh"));
    let mut prs = GhPullRequestReader::new(PathBuf::from("gh"));
    let mut launcher = NativeAmendedSupervisorLauncher;
    let mut local = OsContinuationLocalInspector;
    let mut processes = OsContinuationProcessInspector;
    let result = amend_never_dispatched_initial_branch(
        &mut store,
        AmendedContinuationDependencies {
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
    );
    match result {
        Ok(ContinuationResult::Dispatched(plan)) => println!(
            "{}",
            json!({"command":"amend-undispatched","task_id":args[1],"attempt_id":args[3],
                "config_revision":plan.config_revision,"current_config_revision":args[7],
                "status":"amended_dispatched"})
        ),
        Ok(ContinuationResult::Held(reason)) => {
            println!(
                "{}",
                json!({"command":"amend-undispatched","task_id":args[1],
                "attempt_id":args[3],"status":"held","reason":reason})
            );
            return Err("amendment refused or held".into());
        }
        Err(_) => {
            println!(
                "{}",
                json!({"command":"amend-undispatched","task_id":args[1],
                "attempt_id":args[3],"status":"failed","reason":"state_unavailable"})
            );
            return Err("amendment could not be completed".into());
        }
    }
    Ok(())
}
