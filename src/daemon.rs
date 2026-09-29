use crate::{
    claim::GhAssignmentWriter,
    config::Config,
    coordinator::{
        AttemptReview, OsIdCreator, ProductionLauncher, ScheduleDependencies, schedule_candidates,
    },
    eligibility::{self, Candidate},
    github::{
        identity::verify_authenticated_account, project::GhProjectReader,
        pull_request::GhPullRequestReader,
    },
    state::StateStore,
};
use serde_json::json;
use std::{collections::HashSet, fs, path::PathBuf, thread, time::Duration};

type Error = Box<dyn std::error::Error>;
const POLL_INTERVAL: Duration = Duration::from_secs(30);

struct Options {
    config: PathBuf,
    revision: String,
    targets: Option<(String, Vec<u64>)>,
    once: bool,
    execute: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, Error> {
        let mut config = None;
        let mut revision = None;
        let mut repository = None;
        let mut issues = None;
        let mut once = false;
        let mut execute = false;
        let mut index = 0;
        while index < args.len() {
            let flag = args[index].as_str();
            if matches!(flag, "--once" | "--execute") {
                let slot = if flag == "--once" {
                    &mut once
                } else {
                    &mut execute
                };
                if *slot {
                    return Err("duplicate daemon flag".into());
                }
                *slot = true;
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .filter(|v| !v.is_empty() && !v.starts_with('-'))
                .ok_or("daemon option requires a value")?;
            let slot = match flag {
                "--config" => &mut config,
                "--config-revision" => &mut revision,
                "--repository" => &mut repository,
                "--issues" => &mut issues,
                _ => return Err("unknown daemon option".into()),
            };
            if slot.replace(value.clone()).is_some() {
                return Err("duplicate daemon option".into());
            }
            index += 2;
        }
        let config = PathBuf::from(config.ok_or("daemon requires --config")?);
        let revision = revision.ok_or("daemon requires --config-revision")?;
        if revision.trim().is_empty() {
            return Err("invalid config revision".into());
        }
        let targets = match (repository, issues) {
            (None, None) => None,
            (Some(repo), Some(list)) => {
                let mut numbers = Vec::new();
                let mut seen = HashSet::new();
                for token in list.split(',') {
                    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit()) {
                        return Err("invalid issue list".into());
                    }
                    let number: u64 = token.parse()?;
                    if number == 0 || !seen.insert(number) {
                        return Err("invalid or duplicate issue number".into());
                    }
                    numbers.push(number);
                }
                Some((repo, numbers))
            }
            _ => return Err("--repository and --issues must be supplied together".into()),
        };
        Ok(Self {
            config,
            revision,
            targets,
            once,
            execute,
        })
    }
}

fn candidates(
    reader: &mut GhProjectReader,
    config: &Config,
    targets: Option<&(String, Vec<u64>)>,
) -> Result<Vec<Candidate>, Error> {
    let mut selected = Vec::new();
    if let Some((repository, numbers)) = targets {
        if !config
            .mappings
            .iter()
            .any(|m| &m.tracker_repository == repository)
            || !config
                .sources
                .iter()
                .any(|s| s.repositories.contains(repository))
        {
            return Err("target repository is not configured".into());
        }
        for number in numbers {
            let mut matches = eligibility::select_target(
                reader,
                &config.sources,
                &config.mappings,
                repository,
                *number,
            )?;
            if matches.len() != 1 {
                return Err(format!("target {repository}#{number} selected {} eligible candidates; exactly one required", matches.len()).into());
            }
            selected.append(&mut matches);
        }
    } else {
        selected = eligibility::select(reader, &config.sources, &config.mappings)?;
    }
    let mut identities = HashSet::new();
    let mut numbers = HashSet::new();
    for candidate in &selected {
        if !identities.insert((&candidate.tracker_repo_id, &candidate.issue_node_id))
            || !numbers.insert((&candidate.repository, candidate.issue_number))
        {
            return Err("duplicate selected issue".into());
        }
    }
    selected.sort_by(|a, b| (&a.repository, a.issue_number).cmp(&(&b.repository, b.issue_number)));
    Ok(selected)
}

fn cycle(config: &Config, options: &Options) -> Result<(), Error> {
    let gh = PathBuf::from("gh");
    let mut projects = GhProjectReader::new(gh.clone());
    let selected = candidates(&mut projects, config, options.targets.as_ref())?;
    if !options.execute {
        println!(
            "{}",
            json!({"mode":"preview","candidates":selected.iter().map(|c| json!({
            "repository":c.repository,"issue_number":c.issue_number,"issue_url":c.issue_url,
            "project_id":c.project_id,"item_id":c.item_id
        })).collect::<Vec<_>>()})
        );
        return Ok(());
    }
    for candidate in &selected {
        verify_authenticated_account(&gh, candidate)?;
    }
    let mut store = StateStore::open(&config.state_root, config.capacity)?;
    let mut prs = GhPullRequestReader::new(gh.clone());
    let mut assignments = GhAssignmentWriter { executable: gh };
    let mut launcher = ProductionLauncher;
    let mut ids = OsIdCreator;
    let result = schedule_candidates(
        &mut store,
        selected,
        ScheduleDependencies {
            config,
            config_revision: &options.revision,
            projects: &mut projects,
            prs: &mut prs,
            assignments: &mut assignments,
            launcher: &mut launcher,
            ids: &mut ids,
        },
    );
    // Keep task and attempt identities visible without echoing prompts, shell stderr or API responses.
    let report = result.map_err(|_| "daemon scheduling failed; inspect local task state")?;
    let attempts = report
        .startup
        .attempts
        .iter()
        .map(|a| {
            let status = match a.review {
                AttemptReview::Running => "running",
                AttemptReview::Completed(_) => "completed",
                AttemptReview::Held(_) => "held",
                AttemptReview::Error(_) => "error",
            };
            json!({"task_id":a.task_id,"attempt_id":a.attempt_id,"status":status})
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        json!({"mode":"execute", "reconciliation":attempts,
        "source_holds":report.startup.source_holds.iter().map(|h| json!({"task_id":h.task_id,"kind":h.kind})).collect::<Vec<_>>(),
        "launched":report.launched.iter().map(|p| json!({"task_id":p.task_id,"attempt_id":p.attempt_id})).collect::<Vec<_>>(),
        "skipped_existing":report.skipped_existing.len(), "capacity_full":report.capacity_full})
    );
    if report
        .startup
        .attempts
        .iter()
        .any(|a| matches!(a.review, AttemptReview::Error(_)))
    {
        return Err("reconciliation failed; scheduling blocked".into());
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<(), Error> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "Usage: luthor daemon --config PATH --config-revision REV [--repository owner/repo --issues N,N,...] [--once] [--execute]"
        );
        return Ok(());
    }
    let options = Options::parse(args)?;
    let config = Config::from_json(&fs::read_to_string(&options.config)?)?;
    loop {
        cycle(&config, &options)?;
        if options.once {
            return Ok(());
        }
        thread::sleep(POLL_INTERVAL);
    }
}
