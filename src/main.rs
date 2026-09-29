use luthor::{config::Config, eligibility, github::project::GhProjectReader};
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
    let mut reader = GhProjectReader::new(PathBuf::from("gh"));
    let candidates = eligibility::select(&mut reader, &config.sources, &config.mappings)?;
    let mut output_lines = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let output = json!({
            "candidate": candidate,
            "evidence": {
                "state": candidate.observed_state,
                "assignees": candidate.observed_assignees,
                "labels": candidate.observed_labels,
                "project_fields": candidate.observed_project_fields,
                "eligibility": "selected",
            }
        });
        output_lines.push(serde_json::to_string(&output)?);
    }
    for line in output_lines {
        println!("{line}");
    }
    Ok(())
}
