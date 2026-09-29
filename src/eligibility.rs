use crate::{
    config::{Mapping, Marker, Source},
    github::project::{Issue, ProjectError, ProjectItem, ProjectReader, enumerate},
};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub project_id: String,
    pub item_id: String,
    pub repository: String,
    pub issue_node_id: String,
    pub issue_number: u64,
    pub issue_url: String,
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
}

pub fn select<R: ProjectReader>(
    reader: &mut R,
    sources: &[Source],
    mappings: &[Mapping],
) -> Result<Vec<Candidate>, EligibilityError> {
    let mapping_by_repo: HashMap<_, _> = mappings
        .iter()
        .map(|m| (m.tracker_repository.as_str(), m))
        .collect();
    let mut candidates: HashMap<String, (Candidate, Marker, Option<String>)> = HashMap::new();
    for source in sources {
        for (item, issue) in enumerate(reader, &source.project_id)? {
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
            let key = format!("{}:{}", issue.repository, issue.node_id);
            let candidate = Candidate {
                project_id: source.project_id.clone(),
                item_id: item.item_id,
                repository: issue.repository.clone(),
                issue_node_id: issue.node_id.clone(),
                issue_number: issue.number,
                issue_url: issue.url.clone(),
                mapping: (*mapping).clone(),
                source: source.clone(),
            };
            if let Some((previous, marker, milestone)) = candidates.get(&key) {
                if previous.mapping != candidate.mapping
                    || *marker != source.ready_marker
                    || *milestone != source.milestone
                {
                    return Err(EligibilityError::ConflictingSource(key));
                }
            } else {
                candidates.insert(
                    key,
                    (
                        candidate,
                        source.ready_marker.clone(),
                        source.milestone.clone(),
                    ),
                );
            }
        }
    }
    Ok(candidates
        .into_values()
        .map(|(candidate, _, _)| candidate)
        .collect())
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
