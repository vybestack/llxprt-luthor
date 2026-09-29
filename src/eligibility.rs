use crate::{
    config::{Mapping, Marker, Source},
    github::project::{Issue, ProjectError, ProjectItem, ProjectReader, enumerate},
};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub project_id: String,
    pub item_id: String,
    pub repository: String,
    pub issue_node_id: String,
    pub issue_number: u64,
    pub mapping: MappingIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingIdentity {
    pub code_repository: String,
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
    let observed = enumerate(reader)?;
    let mapping_by_repo: HashMap<_, _> = mappings
        .iter()
        .map(|m| (m.tracker_repository.as_str(), m))
        .collect();
    let mut candidates: HashMap<String, Candidate> = HashMap::new();
    let mut rules = HashSet::new();
    for source in sources {
        for (item, issue) in &observed {
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
                || !marker_matches(&source.ready_marker, item, issue)
                || source
                    .milestone
                    .as_ref()
                    .is_some_and(|required| issue.milestone.as_ref() != Some(required))
            {
                continue;
            }
            let key = format!("{}:{}", issue.repository, issue.node_id);
            let rule = format!(
                "{}|{}|{:?}|{:?}|{}",
                source.project_id,
                issue.repository,
                source.ready_marker,
                source.milestone,
                mapping.code_repository
            );
            if !rules.insert((key.clone(), rule.clone())) && !candidates.contains_key(&key) {
                continue;
            }
            let candidate = Candidate {
                project_id: source.project_id.clone(),
                item_id: item.item_id.clone(),
                repository: issue.repository.clone(),
                issue_node_id: issue.node_id.clone(),
                issue_number: issue.number,
                mapping: MappingIdentity {
                    code_repository: mapping.code_repository.clone(),
                },
            };
            if let Some(previous) = candidates.get(&key) {
                if previous.project_id != candidate.project_id
                    || previous.mapping != candidate.mapping
                    || previous.item_id != candidate.item_id
                {
                    return Err(EligibilityError::ConflictingSource(key));
                }
            } else {
                candidates.insert(key, candidate);
            }
        }
    }
    Ok(candidates.into_values().collect())
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
