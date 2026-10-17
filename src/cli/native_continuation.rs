use crate::{
    cli_identifiers::valid_attempt,
    config::Config,
    coordinator::{NativeContinuationDependencies, ProductionLauncher, continue_amended},
    github::{project::GhProjectReader, pull_request::GhPullRequestReader},
    state::{StateStore, native_continuation::Mode},
};
use std::{fs, path::PathBuf};

pub(crate) fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let flags = [
        "--task",
        "--predecessor",
        "--attempt",
        "--mode",
        "--config",
        "--config-revision",
        "--actor",
    ];
    if args.len() != 15 || args[14] != "--execute" {
        return Err("expected --task TASK --predecessor ID --attempt ID --mode stopped-resume|natural-exit-retry --config PATH --config-revision REV --actor LOGIN --execute".into());
    }
    for (pair, flag) in args[..14].as_chunks::<2>().0.iter().zip(flags) {
        if pair[0] != flag || pair[1].trim().is_empty() || pair[1].starts_with('-') {
            return Err("invalid amended continuation arguments".into());
        }
    }
    if [1, 3, 5, 13].into_iter().any(|i| !valid_attempt(&args[i])) {
        return Err("invalid identifier".into());
    }
    let mode = match args[7].as_str() {
        "stopped-resume" => Mode::StoppedResume,
        "natural-exit-retry" => Mode::NaturalExitRetry,
        _ => return Err("invalid continuation mode".into()),
    };
    let config = Config::from_json(&fs::read_to_string(&args[9])?)?;
    let mut store = StateStore::open(&config.state_root, config.capacity)?;
    let result = continue_amended(
        &mut store,
        NativeContinuationDependencies {
            task_id: &args[1],
            previous_attempt_id: &args[3],
            attempt_id: &args[5],
            mode,
            config: &config,
            config_revision: &args[11],
            actor: &args[13],
            projects: &mut GhProjectReader::new(PathBuf::from("gh")),
            prs: &mut GhPullRequestReader::new(PathBuf::from("gh")),
            launcher: &mut ProductionLauncher,
        },
    );
    println!(
        "{}",
        serde_json::json!({"command":"continue-amended","task_id":args[1],"attempt_id":args[5],"status":if result.is_ok() { "dispatched" } else { "held" }})
    );
    result
        .map(|_| ())
        .map_err(|error| format!("amended continuation held: {error}").into())
}
