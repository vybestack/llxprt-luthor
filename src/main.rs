use luthor::{
    claim::GhAssignmentWriter,
    config::Config,
    coordinator::{
        DispatchDependencies, ProductionLauncher, ResumeDependencies, dispatch_one, resume_one,
    },
    eligibility,
    github::{
        identity::verify_authenticated_account, project::GhProjectReader,
        pull_request::GhPullRequestReader,
    },
    state::StateStore,
};
use serde_json::json;
use std::{env, fs, io::Read, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("luthor: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("__supervise") => {
            let root = args.next().ok_or("missing state root")?;
            let attempt = args.next().ok_or("missing attempt id")?;
            if args.next().is_some() {
                return Err("unexpected argument".into());
            }
            luthor::supervisor::supervise(std::path::Path::new(&root), &attempt)?;
            Ok(())
        }
        #[cfg(unix)]
        Some("__worker_gate") => {
            let plan = args.next().ok_or("missing gate plan")?;
            if args.next().is_some() {
                return Err("unexpected argument".into());
            }
            luthor::supervisor::worker_gate(std::path::Path::new(&plan))?;
            Ok(())
        }
        Some("status" | "show" | "logs") => operator(args),
        Some(command @ ("pause" | "reconcile")) => mutate(command, args.collect()),
        Some("discover") => discover(args.collect()),
        Some("daemon") => luthor::daemon::run(&args.collect::<Vec<_>>()),
        Some("dispatch") => dispatch(args.collect()),
        Some("resume") => resume(args.collect()),
        Some("--help" | "-h") => {
            println!(
                "Usage: luthor discover --config <path>\n       luthor daemon --config PATH --config-revision REV [--repository owner/repo --issues N,N,...] [--once] [--execute]\n       luthor dispatch --config <path> --repository owner/repo --issue N --config-revision REV [--execute]\n       luthor resume TASK --config <path> --execute\n       luthor status --config <path>\n       luthor show TASK --config <path>\n       luthor logs TASK [--attempt ATTEMPT] --config <path>"
            );
            Ok(())
        }
        _ => Err("expected `discover`, `daemon`, `dispatch`, or `resume`".into()),
    }
}

fn option(args: &[String], name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let positions: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| *arg == name)
        .collect();
    if positions.len() != 1 {
        return Err(format!("requires exactly one {name}").into());
    }
    let index = positions[0].0;
    args.get(index + 1)
        .filter(|v| !v.starts_with('-'))
        .cloned()
        .ok_or_else(|| format!("requires value for {name}").into())
}
fn operator(args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
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
    let output = luthor::cli::execute(&config.state_root, &values)?;
    println!("{output}");
    Ok(())
}

fn discover(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
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
fn mutate(command: &str, mut values: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
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
    let config = Config::from_json(&fs::read_to_string(&values[1])?)?;
    let mut store = StateStore::open(&config.state_root, config.capacity)?;
    if store.task_phase(&task_id)?.is_none() {
        return Err("task not found".into());
    }
    let attempt = match attempt_id {
        Some(id) => id,
        None => store.latest_attempt(&task_id)?.ok_or("attempt not found")?,
    };
    if !store.has_attempt(&task_id, &attempt)? {
        return Err("attempt not found for task".into());
    }
    match command {
        "pause" => {
            luthor::supervisor::request_stop(&mut store, &task_id, &attempt)?;
            println!(
                "{}",
                json!({"task_id":task_id,"attempt_id":attempt,"status":"stop_requested"})
            );
        }
        "reconcile" => {
            let mut prs = GhPullRequestReader::new(PathBuf::from("gh"));
            let result =
                luthor::coordinator::reconcile_with_pr(&mut store, &task_id, &attempt, &mut prs)?;
            match result {
                luthor::supervisor::Reconciliation::Completed { exit_code, signal } => println!(
                    "{}",
                    json!({"task_id":task_id,"attempt_id":attempt,"status":"completed","exit_code":exit_code,"signal":signal})
                ),
                luthor::supervisor::Reconciliation::Held { reason } => println!(
                    "{}",
                    json!({"task_id":task_id,"attempt_id":attempt,"status":"held","reason":reason})
                ),
            }
        }
        _ => return Err("invalid control command".into()),
    }
    Ok(())
}

fn dispatch(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
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
    let path = option(&args, "--config")?;
    let repository = option(&args, "--repository")?;
    let issue: u64 = option(&args, "--issue")?.parse()?;
    if issue == 0 {
        return Err("issue number must be positive".into());
    }
    let revision = option(&args, "--config-revision")?;
    if revision.trim().is_empty() || revision.starts_with('-') {
        return Err("invalid config revision".into());
    }
    let config = Config::from_json(&fs::read_to_string(path)?)?;
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
    if execute == 0 {
        return Err("dispatch held: pass --execute to authorize GitHub writes".into());
    }
    verify_authenticated_account(PathBuf::from("gh").as_path(), &config, candidate)?;
    let (task_id, attempt_id) = (random_id()?, random_id()?);
    let mut store = StateStore::open(&config.state_root, config.capacity)?;
    let mut prs = GhPullRequestReader::new(PathBuf::from("gh"));
    let mut assignments = GhAssignmentWriter {
        executable: PathBuf::from("gh"),
    };
    let mut launcher = ProductionLauncher;
    match dispatch_one(
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
    ) {
        Ok(_) => println!(
            "{}",
            json!({"task_id": task_id, "attempt_id": attempt_id, "status": "dispatched"})
        ),
        Err(error) => {
            let status = if store.task_phase(&task_id)?.as_deref() == Some("held") {
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

fn safe_error_stage(error: &luthor::coordinator::DispatchError) -> &'static str {
    use luthor::coordinator::DispatchError;
    match error {
        DispatchError::State(_) => "state transition failed",
        DispatchError::Claim(_) => "claim failed",
        DispatchError::Worktree(_) => "worktree failed",
        DispatchError::ChangedClaim => "claim changed",
        DispatchError::ExistingPr => "pull request exists",
        DispatchError::PullRequest(_) => "pull request lookup failed",
        DispatchError::Supervisor(_) => "worker launch failed",
    }
}

fn random_id() -> Result<String, std::io::Error> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn resume(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
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
    if store.task_phase(task_id)?.is_none() {
        return Err("task not found".into());
    }
    store
        .resume_context(task_id)
        .map_err(|_| "resume held: task is not resumable")?;
    let selection = store
        .selection_evidence(task_id)?
        .ok_or("task selection missing")?;
    verify_authenticated_account(PathBuf::from("gh").as_path(), &config, &selection.candidate)?;
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
            let status = if store.task_phase(task_id)?.as_deref() == Some("held") {
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
