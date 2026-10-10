use super::support::{random_id, safe_error_stage};
use crate::state::task_records;
use crate::{
    config::Config,
    coordinator::ProductionLauncher,
    github::{
        identity::verify_authenticated_account, project::GhProjectReader,
        pull_request::GhPullRequestReader,
    },
    state::StateStore,
};
use serde_json::json;
use std::{fs, path::PathBuf};

pub(crate) fn recover(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() != 10
        || args[1] != "--attempt"
        || args[3] != "--config"
        || args[5] != "--actor"
        || args[7] != "--reason"
        || args[9] != "--execute"
        || args[0].is_empty()
        || args[2].is_empty()
        || args[4].is_empty()
        || args[6].trim().is_empty()
        || args[8].trim().is_empty()
    {
        return Err(
            "expected TASK --attempt ID --config PATH --actor LOGIN --reason TEXT --execute".into(),
        );
    }
    let task_id = &args[0];
    let attempt_id = &args[2];
    let actor = &args[6];
    let reason = &args[8];
    let config = Config::from_json(
        &fs::read_to_string(&args[4]).map_err(|_| "recovery configuration unavailable")?,
    )
    .map_err(|_| "recovery configuration invalid")?;
    let mut store = StateStore::open(&config.state_root, config.capacity)
        .map_err(|_| "recovery state unavailable")?;
    let mut projects = GhProjectReader::new(PathBuf::from("gh"));
    let mut prs = GhPullRequestReader::new(PathBuf::from("gh"));
    let result = crate::coordinator::operator_recover_missing_receipt(
        &mut store,
        task_id,
        attempt_id,
        actor,
        reason,
        &mut projects,
        &mut prs,
    )
    .map_err(|_| "recovery could not be completed")?;
    match result {
        crate::coordinator::RecoveryResult::RecoveredHeld => println!(
            "{}",
            json!({"task_id": task_id, "attempt_id": attempt_id, "status": "recovered_held", "reason": "receipt loss audited; task remains held"})
        ),
        crate::coordinator::RecoveryResult::RecoveredPrComplete { pr_id } => println!(
            "{}",
            json!({"task_id": task_id, "attempt_id": attempt_id, "status": "pr_complete", "pr_id": pr_id})
        ),
        crate::coordinator::RecoveryResult::Held(reason) => println!(
            "{}",
            json!({"task_id": task_id, "attempt_id": attempt_id, "status": "held", "reason": reason})
        ),
    }
    Ok(())
}

pub(crate) fn retry(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let revalidate_terminal_exit = args.len() == 13 && args[11] == "--revalidate-terminal-exit";
    if !(args.len() == 12 || revalidate_terminal_exit)
        || args[1] != "--attempt"
        || args[3] != "--config"
        || args[5] != "--config-revision"
        || args[7] != "--actor"
        || args[9] != "--reason"
        || args.last().map(String::as_str) != Some("--execute")
        || [0, 2, 4, 6, 8, 10]
            .iter()
            .any(|i| args[*i].trim().is_empty() || args[*i].starts_with('-'))
    {
        return Err("expected TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason TEXT [--revalidate-terminal-exit] --execute".into());
    }
    let config = Config::from_json(
        &fs::read_to_string(&args[4]).map_err(|_| "retry configuration unavailable")?,
    )
    .map_err(
        |_| "retry configuration invalid (check worker flags and --max-tool-calls: -1 or 1..512)",
    )?;
    let mut store = StateStore::open(&config.state_root, config.capacity)?;
    crate::state::retry_context_for_task(&store, &args[0], &args[2])
        .map_err(|_| "retry requires the latest verified natural exit in attention")?;
    let selection =
        task_records::selection_evidence(&store, &args[0])?.ok_or("task selection missing")?;
    verify_authenticated_account(PathBuf::from("gh").as_path(), &selection.candidate)?;
    if args[8] != selection.candidate.mapping.allowed_pr_author {
        return Err("retry actor is not the authenticated authorized PR author".into());
    }
    let attempt_id = format!("attempt-{}", random_id()?);
    let mut projects = GhProjectReader::new(PathBuf::from("gh"));
    let mut prs = GhPullRequestReader::new(PathBuf::from("gh"));
    let mut launcher = ProductionLauncher;
    let plan = crate::coordinator::retry_one(
        &mut store,
        crate::coordinator::RetryDependencies {
            task_id: &args[0],
            previous_attempt_id: &args[2],
            attempt_id: &attempt_id,
            config: &config,
            config_revision: &args[6],
            actor: &args[8],
            reason: &args[10],
            revalidate_terminal_exit,
            projects: &mut projects,
            prs: &mut prs,
            launcher: &mut launcher,
        },
    )
    .map_err(|error| format!("retry refused or held: {}", safe_error_stage(&error)))?;
    println!(
        "{}",
        json!({"task_id": plan.task_id, "previous_attempt_id": args[2],
        "attempt_id": plan.attempt_id, "config_revision": plan.config_revision, "status": "retried"})
    );
    Ok(())
}
