type Error = Box<dyn std::error::Error>;
use crate::{
    config::Config,
    eligibility::{self, Candidate, DiscoverySelection},
    github::project::GhProjectReader,
    state::{StateStore, task_records},
};
use std::collections::HashSet;

pub(crate) fn cycle_candidates(
    reader: &mut GhProjectReader,
    config: &Config,
    targets: Option<&(String, Vec<u64>)>,
    store: Option<&StateStore>,
) -> Result<DiscoverySelection, Error> {
    if let Some((repository, _)) = targets {
        validate_target(config, repository)?;
    }
    // A known exhausted quota cannot authorize fresh discovery. Keep local
    // reconciliation and mandatory interrupted-write followup running.
    if !crate::github::budget::usage()
        .exhausted(crate::state::github_budget::now()?)
        .is_empty()
    {
        return Ok(DiscoverySelection::default());
    }
    let mut report = DiscoverySelection::default();
    if let Some((repository, numbers)) = targets {
        for number in numbers {
            let mut selection = eligibility::discover(
                reader,
                &config.sources,
                &config.mappings,
                Some((repository, *number)),
            )?;
            let existing = match store {
                Some(store) => task_records::existing_target(store, repository, *number)?,
                None => false,
            };
            if selection.candidates.is_empty()
                && (!selection.candidate_read_failures.is_empty()
                    || !selection.source_read_failures.is_empty()
                    || !selection.waiting.is_empty()
                    || existing)
            {
                report.waiting.append(&mut selection.waiting);
                report
                    .candidate_read_failures
                    .append(&mut selection.candidate_read_failures);
                report
                    .source_read_failures
                    .append(&mut selection.source_read_failures);
                continue;
            }
            if selection.candidates.len() != 1 {
                return Err(format!("target {repository}#{number} selected {} eligible candidates; exactly one required", selection.candidates.len()).into());
            }
            report.candidates.append(&mut selection.candidates);
            report.waiting.append(&mut selection.waiting);
            report
                .candidate_read_failures
                .append(&mut selection.candidate_read_failures);
            report
                .source_read_failures
                .append(&mut selection.source_read_failures);
        }
    } else {
        report = eligibility::discover(reader, &config.sources, &config.mappings, None)?;
    }
    report.candidates.retain(|candidate| {
        !report
            .source_read_failures
            .iter()
            .any(|failure| failure.project_id == candidate.project_id)
            && !report
                .candidate_read_failures
                .iter()
                .any(|failure| failure.issue == candidate.issue_node_id)
    });
    validate_candidates(&report.candidates)?;
    report
        .candidates
        .sort_by(|a, b| (&a.repository, a.issue_number).cmp(&(&b.repository, b.issue_number)));
    Ok(report)
}

fn validate_target(config: &Config, repository: &str) -> Result<(), Error> {
    if !config
        .mappings
        .iter()
        .any(|m| m.tracker_repository == repository)
        || !config
            .sources
            .iter()
            .any(|s| s.repositories.iter().any(|repo| repo == repository))
    {
        return Err("target repository is not configured".into());
    }
    Ok(())
}

fn validate_candidates(candidates: &[Candidate]) -> Result<(), Error> {
    let mut identities = HashSet::new();
    let mut numbers = HashSet::new();
    for candidate in candidates {
        if !identities.insert((&candidate.tracker_repo_id, &candidate.issue_node_id))
            || !numbers.insert((&candidate.repository, candidate.issue_number))
        {
            return Err("duplicate selected issue".into());
        }
    }
    Ok(())
}
