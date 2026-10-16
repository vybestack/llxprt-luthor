use crate::{
    config::{Mapping, Marker, Source},
    github::project::{Issue, ProjectError, ProjectItem, ProjectReader, enumerate_matching},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub project_id: String,
    pub item_id: String,
    pub repository: String,
    pub issue_node_id: String,
    pub issue_number: u64,
    pub issue_url: String,
    pub tracker_repo_id: String,
    pub milestone_id: Option<String>,
    pub milestone_title: Option<String>,
    pub observed_at_unix_secs: u64,
    pub observed_state: String,
    pub observed_assignees: Vec<String>,
    pub observed_labels: Vec<String>,
    pub observed_project_fields: Vec<(String, String)>,
    pub marker: Marker,
    pub mapping: Mapping,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaitingIssue {
    pub candidate: Candidate,
    pub dependencies: Vec<crate::dependencies::DependencyWait>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EligibilityError {
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error("issue {0} is missing a configured mapping")]
    MissingMapping(String),
    #[error("conflicting source rules for issue {0}")]
    ConflictingSource(String),
    #[error("project {project_id} item {item_id} has unsupported configured marker field {name}")]
    UnsupportedMarkerField {
        project_id: String,
        item_id: String,
        name: String,
    },
}

pub fn select<R: ProjectReader>(
    reader: &mut R,
    sources: &[Source],
    mappings: &[Mapping],
) -> Result<Vec<Candidate>, EligibilityError> {
    let reader = &mut crate::github::project::CandidateReader(reader);
    select_with(reader, sources, mappings, |reader, source| {
        enumerate_matching(reader, &source.project_id, |item| {
            project_candidate(source, mappings, None, item)
        })
    })
}

pub fn select_target<R: ProjectReader>(
    reader: &mut R,
    sources: &[Source],
    mappings: &[Mapping],
    repository: &str,
    issue_number: u64,
) -> Result<Vec<Candidate>, EligibilityError> {
    let reader = &mut crate::github::project::CandidateReader(reader);
    select_with(reader, sources, mappings, |reader, source| {
        enumerate_matching(reader, &source.project_id, |item| {
            project_candidate(source, mappings, Some((repository, issue_number)), item)
        })
    })
}

fn select_with<R, F>(
    reader: &mut R,
    sources: &[Source],
    mappings: &[Mapping],
    mut enumerate_source: F,
) -> Result<Vec<Candidate>, EligibilityError>
where
    R: ProjectReader,
    F: FnMut(&mut R, &Source) -> Result<Vec<(ProjectItem, Issue)>, EligibilityError>,
{
    let mut candidates = HashMap::new();
    for source in sources {
        add_candidates(
            &mut candidates,
            source,
            mappings,
            enumerate_source(reader, source)?,
        )?;
    }
    let mut selected = Vec::new();
    for candidate in candidates.into_values() {
        if crate::dependencies::waiting(
            reader,
            &candidate.source.dependencies,
            &candidate.repository,
            candidate.issue_number,
        )
        .map_err(ProjectError::IssueRead)?
        .is_empty()
        {
            selected.push(candidate);
        }
    }
    Ok(selected)
}

fn add_candidates(
    candidates: &mut HashMap<String, Candidate>,
    source: &Source,
    mappings: &[Mapping],
    items: Vec<(ProjectItem, Issue)>,
) -> Result<(), EligibilityError> {
    let mapping_by_repo: HashMap<_, _> = mappings
        .iter()
        .map(|m| (m.tracker_repository.as_str(), m))
        .collect();
    for (item, issue) in items {
        if !source
            .repositories
            .iter()
            .any(|repo| repo == &issue.repository)
        {
            continue;
        }
        let Some(mapping) = mapping_by_repo.get(issue.repository.as_str()) else {
            return Err(EligibilityError::MissingMapping(issue.repository.clone()));
        };
        if let Marker::ProjectField { name, .. } = &source.ready_marker
            && item.unsupported_fields.iter().any(|field| field == name)
        {
            return Err(EligibilityError::UnsupportedMarkerField {
                project_id: source.project_id.clone(),
                item_id: item.item_id,
                name: name.clone(),
            });
        }
        if issue.state != "open"
            || !issue.assignees.is_empty()
            || !marker_matches(&source.ready_marker, &item, &issue.labels)
            || source
                .milestone
                .as_ref()
                .is_some_and(|required| issue.milestone.as_ref() != Some(required))
        {
            continue;
        }
        let candidate = observed_candidate(source, item, issue, mapping);
        merge_candidate(candidates, candidate)?;
    }
    Ok(())
}

fn observed_candidate(
    source: &Source,
    item: ProjectItem,
    issue: Issue,
    mapping: &Mapping,
) -> Candidate {
    Candidate {
        project_id: source.project_id.clone(),
        item_id: item.item_id,
        repository: issue.repository,
        issue_node_id: issue.node_id,
        issue_number: issue.number,
        issue_url: issue.url,
        tracker_repo_id: issue.tracker_repo_id,
        milestone_id: issue.milestone_id,
        milestone_title: issue.milestone,
        observed_at_unix_secs: issue.observed_at_unix_secs,
        observed_state: issue.state,
        observed_assignees: issue.assignees,
        observed_labels: issue.labels,
        observed_project_fields: item.fields,
        marker: source.ready_marker.clone(),
        mapping: mapping.clone(),
        source: source.clone(),
    }
}

fn merge_candidate(
    candidates: &mut HashMap<String, Candidate>,
    candidate: Candidate,
) -> Result<(), EligibilityError> {
    let key = format!("{}:{}", candidate.repository, candidate.issue_node_id);
    if let Some(previous) = candidates.get(&key) {
        if previous.mapping != candidate.mapping
            || previous.marker != candidate.marker
            || previous.source.milestone != candidate.source.milestone
            || dependency_rule(previous) != dependency_rule(&candidate)
        {
            return Err(EligibilityError::ConflictingSource(key));
        }
    } else {
        candidates.insert(key, candidate);
    }
    Ok(())
}

fn dependency_rule(candidate: &Candidate) -> Option<&crate::config::DependencyRule> {
    candidate.source.dependencies.iter().find(|rule| {
        rule.issue.repository == candidate.repository && rule.issue.number == candidate.issue_number
    })
}

fn marker_matches(marker: &Marker, item: &ProjectItem, labels: &[String]) -> bool {
    match marker {
        Marker::Label { name } => labels.iter().any(|label| label == name),
        Marker::ProjectField { name, value } => item
            .fields
            .iter()
            .any(|(field, actual)| field == name && actual == value),
    }
}

/// GraphQL evidence only excludes items; every surviving candidate still needs
/// direct REST confirmation and the normal pre-claim revalidation.
fn project_candidate(
    source: &Source,
    mappings: &[Mapping],
    target: Option<(&str, u64)>,
    item: &ProjectItem,
) -> Result<bool, EligibilityError> {
    if !source.repositories.contains(&item.repository)
        || target
            .is_some_and(|(repo, number)| item.repository != repo || item.issue_number != number)
    {
        return Ok(false);
    }
    if !mappings
        .iter()
        .any(|m| m.tracker_repository == item.repository)
    {
        return Err(EligibilityError::MissingMapping(item.repository.clone()));
    }
    if let Marker::ProjectField { name, .. } = &source.ready_marker
        && item.unsupported_fields.contains(name)
    {
        return Err(EligibilityError::UnsupportedMarkerField {
            project_id: source.project_id.clone(),
            item_id: item.item_id.clone(),
            name: name.clone(),
        });
    }
    let summary = item.summary.as_ref().ok_or_else(|| {
        ProjectError::Read(crate::github::project::ProjectReadError {
            operation: crate::github::project::ReadOperation::ProjectPage,
            project_id: Some(source.project_id.clone()),
            item_id: Some(item.item_id.clone()),
            issue_id: Some(item.issue_node_id.clone()),
            category: crate::github::project::ReadCategory::Malformed,
            status: None,
            code: "missing-project-issue-summary".into(),
        })
    })?;
    Ok(summary.state == "open"
        && summary.assignees.is_empty()
        && marker_matches(&source.ready_marker, item, &summary.labels)
        && source
            .milestone
            .as_ref()
            .is_none_or(|required| summary.milestone.as_ref() == Some(required)))
}

/// Cycle discovery retains external failures as bounded evidence and never selects
/// an unreadable candidate or a partially enumerated source.
#[derive(Debug, Default)]
pub struct DiscoverySelection {
    pub candidates: Vec<Candidate>,
    pub waiting: Vec<WaitingIssue>,
    pub candidate_read_failures: Vec<crate::github::project::CandidateReadFailure>,
    pub source_read_failures: Vec<crate::github::project::SourceReadFailure>,
}

pub fn discover<R: ProjectReader>(
    reader: &mut R,
    sources: &[Source],
    mappings: &[Mapping],
    target: Option<(&str, u64)>,
) -> Result<DiscoverySelection, EligibilityError> {
    let reader = &mut crate::github::project::CandidateReader(reader);
    let mut report = DiscoverySelection::default();
    let mut candidates = HashMap::new();
    for source in sources {
        let reads = crate::github::project::discover(reader, &source.project_id, |item| {
            project_candidate(source, mappings, target, item)
        })?;
        add_candidates(&mut candidates, source, mappings, reads.items)?;
        report
            .candidate_read_failures
            .extend(reads.candidate_read_failures);
        report
            .source_read_failures
            .extend(reads.source_read_failures);
    }
    // An identity observed elsewhere must not bypass a failed source's rules.
    candidates.retain(|_, candidate| {
        !report
            .candidate_read_failures
            .iter()
            .any(|failure| failure.issue == candidate.issue_node_id)
            && !report
                .source_read_failures
                .iter()
                .any(|failure| failure.project_id == candidate.project_id)
    });
    for candidate in candidates.into_values() {
        match crate::dependencies::waiting(
            reader,
            &candidate.source.dependencies,
            &candidate.repository,
            candidate.issue_number,
        ) {
            Ok(dependencies) if dependencies.is_empty() => report.candidates.push(candidate),
            Ok(dependencies) => report.waiting.push(WaitingIssue {
                candidate,
                dependencies,
            }),
            Err(error) if crate::github::project::external_read_failure(&error) => {
                report
                    .candidate_read_failures
                    .push(crate::github::project::CandidateReadFailure {
                        project_id: candidate.project_id,
                        item: candidate.item_id,
                        issue: candidate.issue_node_id,
                        http_status: error.status,
                        code: error.code,
                    });
            }
            Err(error) => return Err(ProjectError::IssueRead(error).into()),
        }
    }
    report.waiting.sort_by(|a, b| {
        (&a.candidate.repository, a.candidate.issue_number)
            .cmp(&(&b.candidate.repository, b.candidate.issue_number))
    });
    Ok(report)
}
