use crate::state::task_records;
use crate::{
    config::Config,
    eligibility,
    github::{project::GhProjectReader, pull_request::GhPullRequestReader},
    state::StateStore,
};
use serde_json::json;
use std::{fs, path::PathBuf};

struct ControlArguments {
    task_id: String,
    attempt_id: Option<String>,
    config_path: String,
}

pub(crate) fn operator(
    args: impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut values = args.collect::<Vec<_>>();
    let config_pos = values
        .iter()
        .position(|arg| arg == "--config")
        .ok_or("requires --config <path>")?;
    if values.len() <= config_pos + 1 {
        return Err("requires --config <path>".into());
    }
    let path = values.remove(config_pos + 1);
    values.remove(config_pos);
    let config = Config::from_json(&fs::read_to_string(path)?)?;
    let output = crate::cli::execute(&config.state_root, &values)?;
    println!("{output}");
    Ok(())
}

pub(crate) fn discover(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: luthor discover --config <path>");
        return Ok(());
    }
    if args.len() != 2 || args[0] != "--config" {
        return Err("discover requires --config <path>".into());
    }
    let config = Config::from_json(&fs::read_to_string(&args[1])?)?;
    let mut reader = GhProjectReader::new(PathBuf::from("gh"));
    let candidates = eligibility::select(&mut reader, &config.sources, &config.mappings)?;
    let lines = candidates.into_iter().map(|candidate| {
        let output = json!({"candidate": candidate, "evidence": {"state": candidate.observed_state, "assignees": candidate.observed_assignees, "labels": candidate.observed_labels, "project_fields": candidate.observed_project_fields, "eligibility": "selected"}});
        serde_json::to_string(&output)
    }).collect::<Result<Vec<_>, _>>()?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

fn control_arguments(
    command: &str,
    mut values: Vec<String>,
) -> Result<ControlArguments, Box<dyn std::error::Error>> {
    let task_id = values.first().cloned().ok_or("task id is required")?;
    values.remove(0);
    let mut attempt_id = None;
    if command == "reconcile" && values.first().is_some_and(|value| value == "--attempt") {
        values.remove(0);
        attempt_id = Some(values.first().cloned().ok_or("attempt id is required")?);
        values.remove(0);
    }
    if values.len() != 2 || values[0] != "--config" || values[1].starts_with('-') {
        return Err("expected TASK [--attempt ID] --config PATH".into());
    }
    Ok(ControlArguments {
        task_id,
        attempt_id,
        config_path: values.remove(1),
    })
}

pub(crate) fn mutate(command: &str, values: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let ControlArguments {
        task_id,
        attempt_id,
        config_path,
    } = control_arguments(command, values)?;
    let config = Config::from_json(&fs::read_to_string(config_path)?)?;
    let mut store = StateStore::open(&config.state_root, config.capacity)?;
    if task_records::task_phase(&store, &task_id)?.is_none() {
        return Err("task not found".into());
    }
    let latest = task_records::latest_attempt(&store, &task_id)?;
    if command == "reconcile" && latest.is_none() && attempt_id.is_none() {
        let mut projects = GhProjectReader::new(PathBuf::from("gh"));
        let report = crate::coordinator::reconcile_source(&mut store, &task_id, &mut projects)?;
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    let attempt = match attempt_id {
        Some(id) => id,
        None => latest.ok_or("attempt not found")?,
    };
    if !task_records::has_attempt(&store, &task_id, &attempt)? {
        return Err("attempt not found for task".into());
    }
    match command {
        "pause" => {
            crate::supervisor::request_stop(&mut store, &task_id, &attempt)?;
            println!(
                "{}",
                json!({"task_id":task_id,"attempt_id":attempt,"status":"stop_requested"})
            );
        }
        "reconcile" => {
            let mut projects = GhProjectReader::new(PathBuf::from("gh"));
            let mut prs = GhPullRequestReader::new(PathBuf::from("gh"));
            let result = crate::coordinator::reconcile_with_pr(
                &mut store,
                &task_id,
                &attempt,
                &mut projects,
                &mut prs,
            )?;
            match result {
                crate::supervisor::Reconciliation::Running => println!(
                    "{}",
                    json!({"task_id":task_id,"attempt_id":attempt,"status":"running"})
                ),
                crate::supervisor::Reconciliation::Completed { exit_code, signal } => println!(
                    "{}",
                    json!({"task_id":task_id,"attempt_id":attempt,"status":"completed","exit_code":exit_code,"signal":signal})
                ),
                crate::supervisor::Reconciliation::Held { reason } => println!(
                    "{}",
                    json!({"task_id":task_id,"attempt_id":attempt,"status":"held","reason":reason})
                ),
            }
        }
        _ => return Err("invalid control command".into()),
    }
    Ok(())
}
