use crate::{config::Config, eligibility, github::project::GhProjectReader};
use serde_json::{Value, json};
use std::path::PathBuf;

type Error = Box<dyn std::error::Error>;

pub(crate) fn discovery(config: &Config) -> Result<Vec<Value>, Error> {
    let mut reader = GhProjectReader::new(PathBuf::from("gh"));
    let report = eligibility::discover(&mut reader, &config.sources, &config.mappings, None)?;
    let mut lines: Vec<_> = report.candidates.into_iter().map(|candidate| {
        json!({"candidate":candidate,"evidence":{
            "state":candidate.observed_state,"assignees":candidate.observed_assignees,
            "labels":candidate.observed_labels,"project_fields":candidate.observed_project_fields,
            "eligibility":"selected"
        }})
    }).collect();
    lines.extend(report.waiting.into_iter().map(|waiting| {
        json!({"candidate":waiting.candidate,"evidence":{
            "eligibility":"waiting","dependencies":waiting.dependencies
        }})
    }));
    for failure in report.candidate_read_failures {
        lines.push(json!({"candidate_read_failure":failure}));
    }
    for failure in report.source_read_failures {
        lines.push(json!({"source_read_failure":failure}));
    }
    Ok(lines)
}

pub(crate) fn status(config: &Config, output: &str) -> Result<Value, Error> {
    let mut output: Value = serde_json::from_str(output)?;
    let mut waiting = Vec::new();
    if config
        .sources
        .iter()
        .any(|source| !source.dependencies.is_empty())
    {
        let mut reader = GhProjectReader::new(PathBuf::from("gh"));
        let report = eligibility::discover(&mut reader, &config.sources, &config.mappings, None)?;
        waiting = report.waiting;
        output["dependency_candidate_read_failures"] = json!(report.candidate_read_failures);
        output["dependency_source_read_failures"] = json!(report.source_read_failures);
    }
    output["waiting"] = json!(waiting);
    Ok(output)
}
