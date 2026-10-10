use crate::{
    config::{Mapping, Marker, Source},
    github::project::{
        Issue, ProjectError, ProjectItem, ProjectReader, enumerate, enumerate_target,
    },
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
    let repositories: Vec<_> = sources
        .iter()
        .flat_map(|source| source.repositories.iter().cloned())
        .collect();
    select_with(reader, sources, mappings, |reader, source| {
        enumerate(reader, &source.project_id, &repositories)
    })
}

pub fn select_target<R: ProjectReader>(
    reader: &mut R,
    sources: &[Source],
    mappings: &[Mapping],
    repository: &str,
    issue_number: u64,
) -> Result<Vec<Candidate>, EligibilityError> {
    select_with(reader, sources, mappings, |reader, source| {
        enumerate_target(reader, &source.project_id, repository, issue_number)
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
    F: FnMut(&mut R, &Source) -> Result<Vec<(ProjectItem, Issue)>, ProjectError>,
{
    let mapping_by_repo: HashMap<_, _> = mappings
        .iter()
        .map(|m| (m.tracker_repository.as_str(), m))
        .collect();
    let mut candidates = HashMap::new();
    for source in sources {
        for (item, issue) in enumerate_source(reader, source)? {
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
                || !marker_matches(&source.ready_marker, &item, &issue)
                || source
                    .milestone
                    .as_ref()
                    .is_some_and(|required| issue.milestone.as_ref() != Some(required))
            {
                continue;
            }
            let candidate = observed_candidate(source, item, issue, mapping);
            merge_candidate(&mut candidates, candidate)?;
        }
    }
    Ok(candidates.into_values().collect())
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
        {
            return Err(EligibilityError::ConflictingSource(key));
        }
    } else {
        candidates.insert(key, candidate);
    }
    Ok(())
}

fn marker_matches(marker: &Marker, item: &ProjectItem, issue: &Issue) -> bool {
    match marker {
        Marker::Label { name } => issue.labels.iter().any(|label| label == name),
        Marker::ProjectField { name, value } => item
            .fields
            .iter()
            .any(|(field, actual)| field == name && actual == value),
    }
}
