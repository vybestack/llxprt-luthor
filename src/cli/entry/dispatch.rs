use super::support::{option, random_id, safe_error_stage};
use crate::state::task_records;
use crate::{
    claim::GhAssignmentWriter,
    config::Config,
    coordinator::{DispatchDependencies, ProductionLauncher, dispatch_one, startup_reconcile_all},
    eligibility,
    github::{
        identity::verify_authenticated_account, project::GhProjectReader,
        pull_request::GhPullRequestReader,
    },
    state::StateStore,
};
use serde_json::json;
use std::{fs, path::PathBuf};

struct DispatchArguments {
    execute: bool,
    path: String,
    repository: String,
    issue: u64,
    revision: String,
}

fn arguments(args: &[String]) -> Result<DispatchArguments, Box<dyn std::error::Error>> {
    let execute = args.iter().filter(|arg| *arg == "--execute").count();
    if execute > 1
        || args.len() != if execute == 1 { 9 } else { 8 }
        || args.iter().any(|arg| {
            arg.starts_with("--")
                && ![
                    "--execute",
                    "--config",
                    "--repository",
                    "--issue",
                    "--config-revision",
                ]
                .contains(&arg.as_str())
        })
    {
        return Err("invalid dispatch arguments".into());
    }
    let path = option(args, "--config")?;
    let repository = option(args, "--repository")?;
    let issue: u64 = option(args, "--issue")?.parse()?;
    if issue == 0 {
        return Err("issue number must be positive".into());
    }
    let revision = option(args, "--config-revision")?;
    if revision.trim().is_empty() || revision.starts_with('-') {
        return Err("invalid config revision".into());
    }
    Ok(DispatchArguments {
        execute: execute == 1,
        path,
        repository,
        issue,
        revision,
    })
}

pub(crate) fn dispatch(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let DispatchArguments {
        execute,
        path,
        repository,
        issue,
        revision,
    } = arguments(&args)?;
    let config = Config::from_json(&fs::read_to_string(path)?)?;
    if !execute {
        return Err("dispatch held: pass --execute to authorize GitHub writes".into());
    }
    let mut store = StateStore::open(&config.state_root, config.capacity)?;
    let mut projects = GhProjectReader::new(PathBuf::from("gh"));
    let mut prs = GhPullRequestReader::new(PathBuf::from("gh"));
    reconcile_before_dispatch(&mut store, &mut projects, &mut prs)?;
    let mut projects = GhProjectReader::new(PathBuf::from("gh"));
    let candidates = eligibility::select_target(
        &mut projects,
        &config.sources,
        &config.mappings,
        &repository,
        issue,
    )?;
    if candidates.len() != 1 {
        return Err(format!(
            "target selected {} eligible candidates; exactly one required",
            candidates.len()
        )
        .into());
    }
    let candidate = &candidates[0];
    verify_authenticated_account(PathBuf::from("gh").as_path(), candidate)?;
    let (task_id, attempt_id) = (random_id()?, random_id()?);
    let mut assignments = GhAssignmentWriter {
        executable: PathBuf::from("gh"),
    };
    let mut launcher = ProductionLauncher;
    let result = dispatch_one(
        &mut store,
        candidate,
        DispatchDependencies {
            config: &config,
            config_revision: &revision,
            task_id: &task_id,
            attempt_id: &attempt_id,
            projects: &mut projects,
            prs: &mut prs,
            assignments: &mut assignments,
            launcher: &mut launcher,
        },
    );
    report_dispatch(&store, &task_id, &attempt_id, result.map(|_| ()))?;
    Ok(())
}

fn reconcile_before_dispatch(
    store: &mut StateStore,
    projects: &mut GhProjectReader,
    prs: &mut GhPullRequestReader,
) -> Result<(), Box<dyn std::error::Error>> {
    let startup = startup_reconcile_all(store, projects, prs)?;
    if startup.scheduling_blocked()
        || startup.attempts.iter().any(|attempt| {
            matches!(
                attempt.review,
                crate::coordinator::AttemptReview::Held(_)
                    | crate::coordinator::AttemptReview::Error(_)
            )
        })
    {
        return Err(
            "dispatch held: startup reconciliation has unresolved attempts or source intents"
                .into(),
        );
    }
    Ok(())
}

fn report_dispatch(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    result: Result<(), crate::coordinator::DispatchError>,
) -> Result<(), Box<dyn std::error::Error>> {
    match result {
        Ok(_) => println!(
            "{}",
            json!({"task_id": task_id, "attempt_id": attempt_id, "status": "dispatched"})
        ),
        Err(error) => {
            let status = if task_records::task_phase(store, task_id)?.as_deref() == Some("held") {
                "held"
            } else {
                "failed"
            };
            println!(
                "{}",
                json!({"task_id": task_id, "attempt_id": attempt_id, "status": status})
            );
            return Err(format!("dispatch {status}: {}", safe_error_stage(&error)).into());
        }
    }
    Ok(())
}
