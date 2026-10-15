use crate::{
    claim::GhAssignmentWriter,
    config::Config,
    coordinator::{
        AttemptReview, IdCreator, OsIdCreator, PrRepairReport, ProductionLauncher,
        ScheduleDependencies, ScheduleReport, StartupReport, SupervisorLauncher,
        schedule_candidates_after_startup, startup_reconcile_all,
    },
    github::{
        identity::verify_authenticated_account, project::GhProjectReader,
        pull_request::GhPullRequestReader,
    },
    state::StateStore,
};
use serde_json::json;
use std::{collections::HashSet, fs, path::PathBuf, thread};

type Error = Box<dyn std::error::Error>;

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

mod discovery;
mod polling;
use discovery::cycle_candidates;

fn cycle(config: &Config, options: &Options) -> Result<(), Error> {
    let mut launcher = ProductionLauncher;
    let mut ids = OsIdCreator;
    cycle_with_dependencies(
        config,
        options,
        PathBuf::from("gh"),
        &mut launcher,
        &mut ids,
    )
    .map(|_| ())
}

fn preview(
    projects: &mut GhProjectReader,
    config: &Config,
    options: &Options,
) -> Result<(), Error> {
    let selection = cycle_candidates(projects, config, options.targets.as_ref(), None)?;
    println!(
        "{}",
        json!({"mode":"preview","waiting":selection.waiting,
        "candidate_read_failures":selection.candidate_read_failures,
        "source_read_failures":selection.source_read_failures,
        "candidates":selection.candidates.iter().map(|c| json!({
        "repository":c.repository,"issue_number":c.issue_number,"issue_url":c.issue_url,
        "project_id":c.project_id,"item_id":c.item_id
    })).collect::<Vec<_>>()})
    );
    Ok(())
}

/// Before cleanup and scheduling, so finished tasks stop counting as
/// unresolved capacity: first the ones an earlier release wrongly held go back
/// to cleanup, then tasks whose issue closed or PR finished are retired. This
/// precedes the blocked check, because a finished task's leftover source
/// operation or unreconciled attempt must not stop the queue.
fn settle_finished_tasks(
    store: &mut StateStore,
    projects: &mut GhProjectReader,
    prs: &mut GhPullRequestReader,
    startup: &mut StartupReport,
    config: &Config,
) -> Result<PrRepairReport, Error> {
    let mut restored = crate::coordinator::restore_open_held_prs(store, projects, prs)
        .map_err(|_| "open PR hold restoration failed; inspect local task state")?;
    let terminal = crate::coordinator::restore_wrongly_held_prs(store, prs)
        .map_err(|_| "PR hold restoration failed; inspect local task state")?;
    restored.restored.extend(terminal.restored);
    restored.retained.extend(terminal.retained);
    let retirement =
        crate::coordinator::pr_merge::retire_with_tracker(store, config, projects, prs)
            .map_err(|_| "task retirement failed; inspect local task state")?;
    startup
        .settle_retired(store, &retirement.retired)
        .map_err(|_| "task retirement failed; inspect local task state")?;
    Ok(PrRepairReport {
        restored: restored.restored,
        restore_retained: restored.retained,
        retired: retirement.retired,
        retire_retained: retirement.retained,
        ..Default::default()
    })
}

fn cycle_with_dependencies<L: SupervisorLauncher, I: IdCreator>(
    config: &Config,
    options: &Options,
    gh: PathBuf,
    launcher: &mut L,
    ids: &mut I,
) -> Result<Option<ScheduleReport>, Error> {
    let _github_cycle = crate::github::budget::CycleGuard::start(&config.github_polling);
    let mut projects = GhProjectReader::new(gh.clone());
    if !options.execute {
        preview(&mut projects, config, options)?;
        return Ok(None);
    }
    crate::github::quota::refresh(&gh)?;
    let mut store = StateStore::open(&config.state_root, config.capacity)?;
    let mut prs = GhPullRequestReader::new(gh.clone());
    let (startup, mut pr_repair, operator_launches) =
        reconcile_cycle(&mut store, config, &gh, &mut projects, &mut prs, launcher)?;
    let (pr_merge, cleanup) = sweep_and_clean_prs(&mut store, config, &mut prs)?;
    let disk_space = crate::disk::inspect(&config.worktree_root, config.min_free_space_bytes)?;
    if disk_space.dispatch_held {
        let report = ScheduleReport {
            github: crate::github::budget::usage(),
            startup,
            launched: operator_launches,
            disk_space: Some(disk_space),
            pr_merge,
            pr_repair,
            ..Default::default()
        };
        print_schedule_report(&report, &cleanup);
        return Ok(Some(report));
    }
    let repair = crate::coordinator::repair_verified_prs_after_cleanup(
        &mut store,
        config,
        &mut projects,
        &mut prs,
        launcher,
        ids,
        &cleanup,
    )
    .map_err(|_| "PR repair could not record task state; inspect local task state")?;
    let selection = cycle_candidates(
        &mut projects,
        config,
        options.targets.as_ref(),
        Some(&store),
    )?;
    verify_candidate_accounts(&gh, &selection.candidates)?;
    let mut assignments = GhAssignmentWriter { executable: gh };
    let result = schedule_candidates_after_startup(
        &mut store,
        selection.candidates,
        ScheduleDependencies {
            config,
            config_revision: &options.revision,
            projects: &mut projects,
            prs: &mut prs,
            assignments: &mut assignments,
            launcher,
            ids,
        },
        startup,
    );
    // Keep task and attempt identities visible without echoing prompts, shell stderr or API responses.
    let mut report = result.map_err(|_| "daemon scheduling failed; inspect local task state")?;
    pr_repair.retry_scheduled = repair.retry_scheduled;
    pr_repair.skipped = repair.skipped;
    pr_repair.held = repair.held;
    pr_repair.retired.extend(repair.retired);
    report.waiting.extend(selection.waiting);
    report.candidate_read_failures = selection.candidate_read_failures;
    report.source_read_failures = selection.source_read_failures;
    report.pr_repair = pr_repair;
    finish_pr_report(&mut report, pr_merge, repair.launched, operator_launches);
    print_schedule_report(&report, &cleanup);
    verify_report(&report)?;
    Ok(Some(report))
}

fn reconcile_cycle<L: SupervisorLauncher>(
    store: &mut StateStore,
    config: &Config,
    gh: &std::path::Path,
    projects: &mut GhProjectReader,
    prs: &mut GhPullRequestReader,
    launcher: &mut L,
) -> Result<
    (
        StartupReport,
        PrRepairReport,
        Vec<crate::supervisor::LaunchPlan>,
    ),
    Error,
> {
    let mut startup = startup_reconcile_all(store, projects, prs)?;
    let pr_repair = settle_finished_tasks(store, projects, prs, &mut startup, config)?;
    if startup.scheduling_blocked() {
        println!("{}", json!({"mode":"execute", "blocked":true}));
        return Err(
            polling::ReconciliationBlocked("startup reconciliation blocked scheduling").into(),
        );
    }
    let launched = crate::coordinator::requests::drain(store, config, gh, projects, prs, launcher)?;
    Ok((startup, pr_repair, launched))
}

fn verify_report(report: &ScheduleReport) -> Result<(), Error> {
    if report
        .startup
        .attempts
        .iter()
        .any(|a| matches!(a.review, AttemptReview::Error(_)))
    {
        return Err(
            polling::ReconciliationBlocked("reconciliation failed; scheduling blocked").into(),
        );
    }
    Ok(())
}

fn print_schedule_report(report: &ScheduleReport, cleanup: &crate::cleanup::CleanupReport) {
    let attempts = report
        .startup
        .attempts
        .iter()
        .map(|attempt| {
            let status = match attempt.review {
                AttemptReview::Running => "running",
                AttemptReview::Completed(_) => "completed",
                AttemptReview::Held(_) => "held",
                AttemptReview::Error(_) => "error",
            };
            json!({"task_id":attempt.task_id,"attempt_id":attempt.attempt_id,"status":status})
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        json!({"mode":"execute", "github":report.github, "cleanup":cleanup, "reconciliation":attempts,
        "source_holds":report.startup.source_holds.iter().map(|hold| json!({"task_id":hold.task_id,"kind":hold.kind})).collect::<Vec<_>>(),
        "pr_repair":report.pr_repair, "pr_merge":report.pr_merge, "automatic_retries":report.automatic_retries,
        "waiting":report.waiting,
        "candidate_read_failures":report.candidate_read_failures,
        "source_read_failures":report.source_read_failures,
        "launched":report.launched.iter().map(|plan| json!({"task_id":plan.task_id,"attempt_id":plan.attempt_id})).collect::<Vec<_>>(),
        "skipped_existing":report.skipped_existing.len(), "capacity_full":report.capacity_full,"disk_space":report.disk_space})
    );
}

#[cfg(test)]
mod tests;

pub fn run(args: &[String]) -> Result<(), Error> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "Usage: luthor daemon --config PATH --config-revision REV [--repository owner/repo --issues N,N,...] [--once] [--execute]"
        );
        return Ok(());
    }
    let options = Options::parse(args)?;
    let config = Config::from_json(&fs::read_to_string(&options.config)?)?;
    polling::run(options.once, || cycle(&config, &options), thread::sleep)
}

fn verify_candidate_accounts(
    gh: &std::path::Path,
    candidates: &[crate::eligibility::Candidate],
) -> Result<(), Error> {
    for candidate in candidates {
        verify_authenticated_account(gh, candidate)?;
    }
    Ok(())
}

fn sweep_prs(
    store: &mut StateStore,
    config: &Config,
    prs: &mut GhPullRequestReader,
) -> Result<crate::coordinator::MergeReport, Error> {
    let mut report = crate::coordinator::pr_merge::settle_completed_prs(store, config, prs)?;
    let mut merging = crate::coordinator::merge_verified_prs(store, config, prs)?;
    report.cleanup.append(&mut merging.cleanup);
    report.merged.extend(merging.merged);
    report.waiting.extend(merging.waiting);
    report.delivered.extend(merging.delivered);
    Ok(report)
}

fn finish_pr_report(
    report: &mut ScheduleReport,
    merging: crate::coordinator::MergeReport,
    repairs: Vec<crate::supervisor::LaunchPlan>,
    operators: Vec<crate::supervisor::LaunchPlan>,
) {
    report.launched.splice(0..0, repairs);
    report.launched.splice(0..0, operators);
    report.pr_merge = merging;
    report.github = crate::github::budget::usage();
}

fn sweep_and_clean_prs(
    store: &mut StateStore,
    config: &Config,
    prs: &mut GhPullRequestReader,
) -> Result<
    (
        crate::coordinator::MergeReport,
        crate::cleanup::CleanupReport,
    ),
    Error,
> {
    let mut merging = sweep_prs(store, config, prs)?;
    let mut cleanup = crate::cleanup::clean_terminal_tasks(store, config, prs)
        .map_err(|_| "cleanup failed; inspect local task state")?;
    cleanup.append(&mut merging.cleanup);
    Ok((merging, cleanup))
}
