use serde_json::Value;

mod checks;
mod evidence;
mod reader;
mod types;

#[cfg(test)]
mod evidence_tests;

use evidence::parse_evidence;
pub use reader::GhPullRequestReader;
use types::error;
pub use types::{ErrorCategory, LookupError, LookupResult, PullRequestEvidence, PullRequestReader};

pub fn lookup<R: PullRequestReader>(
    reader: &mut R,
    repository: &str,
    issue_url: &str,
) -> Result<LookupResult, LookupError> {
    let mut matches = Vec::new();
    let mut page_number = 1;
    loop {
        if page_number > 10_000 {
            return Err(error(ErrorCategory::Malformed, "page-limit", None));
        }
        let page = reader.page(repository, page_number)?;
        if page.len() > 100 {
            return Err(error(ErrorCategory::Malformed, "oversized-page", None));
        }
        let short_page = page.len() < 100;
        for item in page {
            let number = item
                .get("number")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0)
                .ok_or_else(|| error(ErrorCategory::Malformed, "invalid-list-entry", None))?;
            let body = item
                .get("body")
                .and_then(Value::as_str)
                .ok_or_else(|| error(ErrorCategory::Malformed, "invalid-list-entry", None))?;
            if !has_tracker_line(body, issue_url) {
                continue;
            }
            let detail = reader.detail(repository, number)?;
            let evidence = parse_evidence(&detail, repository)?;
            if evidence.number != number || evidence.tracker_issue_url != issue_url {
                return Err(error(ErrorCategory::Malformed, "invalid-pr-details", None));
            }
            matches.push(evidence);
        }
        if short_page {
            break;
        }
        page_number = page_number
            .checked_add(1)
            .ok_or_else(|| error(ErrorCategory::Malformed, "page-overflow", None))?;
    }
    Ok(match matches.len() {
        0 => LookupResult::Absent,
        1 => LookupResult::OpenPreexisting(Box::new(matches.remove(0))),
        _ => LookupResult::Ambiguous(matches),
    })
}

fn has_tracker_line(body: &str, issue_url: &str) -> bool {
    body.lines()
        .any(|line| line == format!("Tracker-Issue: {issue_url}"))
}
