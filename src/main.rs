use luthor::{config::Config, eligibility, github::project::GhProjectReader, state::StateStore};
use serde_json::json;
use std::{env, fs, path::PathBuf, process::ExitCode};

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
        Some("discover") => {}
        Some("--help" | "-h") => {
            println!("Usage: luthor discover --config <path>");
            return Ok(());
        }
        _ => return Err("expected `discover --config <path>`".into()),
    }
    let first = args.next();
    if matches!(first.as_deref(), Some("--help" | "-h")) {
        println!("Usage: luthor discover --config <path>");
        return Ok(());
    }
    if first.as_deref() != Some("--config") {
        return Err("discover requires --config <path>".into());
    }
    let path = args.next().ok_or("discover requires a config path")?;
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }

    let config = Config::from_json(&fs::read_to_string(path)?)?;
    let _state = StateStore::open(&config.state_root, config.capacity)?;
    let mut reader = GhProjectReader::new(PathBuf::from("gh"));
    let candidates = eligibility::select(&mut reader, &config.sources, &config.mappings)?;
    let sources: std::collections::HashMap<_, _> = config
        .sources
        .iter()
        .map(|source| (source.project_id.as_str(), source))
        .collect();
    for candidate in candidates {
        let source = sources[candidate.project_id.as_str()];
        let output = json!({
            "candidate": {
                "project_id": candidate.project_id,
                "item_id": candidate.item_id,
                "repository": candidate.repository,
                "issue_node_id": candidate.issue_node_id,
                "issue_number": candidate.issue_number,
                "code_repository": candidate.mapping.code_repository,
            },
            "source": {
                "project_id": source.project_id,
                "ready_marker": format!("{:?}", source.ready_marker),
                "milestone": source.milestone,
            },
            "evidence": {
                "state": "open",
                "assignees": [],
                "eligibility": "selected",
            }
        });
        println!("{}", serde_json::to_string(&output)?);
    }
    Ok(())
}
