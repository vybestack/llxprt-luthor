use super::support::{random_id, safe_error_stage};
use crate::state::{launches, task_records};
use crate::{
    config::Config,
    coordinator::{ProductionLauncher, ResumeDependencies, resume_one},
    github::{
        identity::verify_authenticated_account, project::GhProjectReader,
        pull_request::GhPullRequestReader,
    },
    state::StateStore,
};
use serde_json::json;
use std::{fs, path::PathBuf};

pub(crate) fn resume(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    if !(args.len() == 3 || args.len() == 4)
        || args[1] != "--config"
        || args[2].starts_with('-')
        || (args.len() == 4 && args[3] != "--execute")
    {
        return Err("expected TASK --config PATH [--execute]".into());
    }
    let task_id = &args[0];
    let path = &args[2];
    if args.len() == 3 {
        return Err("resume held: pass --execute to authorize worker launch".into());
    }
    if task_id.is_empty() || task_id.starts_with('-') {
        return Err("invalid task id".into());
    }
    let config = Config::from_json(&fs::read_to_string(path)?)?;
    let mut store = StateStore::open(&config.state_root, config.capacity)?;
    if task_records::task_phase(&store, task_id)?.is_none() {
        return Err("task not found".into());
    }
    launches::resume_context(&store, task_id).map_err(|_| "resume held: task is not resumable")?;
    let selection =
        task_records::selection_evidence(&store, task_id)?.ok_or("task selection missing")?;
    verify_authenticated_account(PathBuf::from("gh").as_path(), &selection.candidate)?;
    let attempt_id = random_id()?;
    let mut projects = GhProjectReader::new(PathBuf::from("gh"));
    let mut prs = GhPullRequestReader::new(PathBuf::from("gh"));
    let mut launcher = ProductionLauncher;
    match resume_one(
        &mut store,
        ResumeDependencies {
            task_id,
            attempt_id: &attempt_id,
            projects: &mut projects,
            prs: &mut prs,
            launcher: &mut launcher,
        },
    ) {
        Ok(_) => println!(
            "{}",
            json!({"task_id":task_id,"attempt_id":attempt_id,"status":"resumed"})
        ),
        Err(error) => {
            let status = if task_records::task_phase(&store, task_id)?.as_deref() == Some("held") {
                "held"
            } else {
                "failed"
            };
            println!(
                "{}",
                json!({"task_id":task_id,"attempt_id":attempt_id,"status":status})
            );
            return Err(format!("resume {status}: {}", safe_error_stage(&error)).into());
        }
    }
    Ok(())
}
