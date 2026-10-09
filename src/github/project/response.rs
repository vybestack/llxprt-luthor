use serde_json::Value;

use super::types::{ProjectItem, ProjectReadError, ReadCategory, ReadOperation};

pub(crate) fn status_value(value: &Value) -> Option<u16> {
    value
        .as_str()
        .and_then(|status| status.parse().ok())
        .or_else(|| value.as_u64().and_then(|status| u16::try_from(status).ok()))
}

pub(crate) fn stderr_status(stderr: &str) -> Option<u16> {
    let words = stderr.split_whitespace().collect::<Vec<_>>();
    words.windows(2).find_map(|pair| {
        if pair[0] == "HTTP" {
            pair[1]
                .trim_matches(|ch: char| !ch.is_ascii_digit())
                .parse()
                .ok()
        } else {
            None
        }
    })
}

pub(crate) fn classify_status(
    status: Option<u16>,
    json_message: &str,
    stderr: &str,
) -> ReadCategory {
    let rate_limited = json_message.to_ascii_lowercase().contains("rate limit")
        || stderr.to_ascii_lowercase().contains("rate limit");
    match status {
        Some(401) => ReadCategory::Permission,
        Some(403) if rate_limited => ReadCategory::RateLimit,
        Some(403) => ReadCategory::Permission,
        Some(404) => ReadCategory::NotFound,
        Some(429) => ReadCategory::RateLimit,
        _ => ReadCategory::Unknown,
    }
}

pub(crate) fn classify_graphql_errors(errors: &[Value]) -> ReadCategory {
    let mut category = ReadCategory::Unknown;
    for error in errors {
        let code = error
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| error.pointer("/extensions/code").and_then(Value::as_str))
            .unwrap_or("")
            .to_ascii_uppercase();
        let candidate = match code.as_str() {
            "RATE_LIMITED" | "RATE_LIMIT" => ReadCategory::RateLimit,
            "FORBIDDEN" | "UNAUTHORIZED" => ReadCategory::Permission,
            "NOT_FOUND" => ReadCategory::NotFound,
            _ => {
                let message = error.get("message").and_then(Value::as_str).unwrap_or("");
                if message.len() <= 256 && message.to_ascii_lowercase().contains("rate limit") {
                    ReadCategory::RateLimit
                } else {
                    ReadCategory::Unknown
                }
            }
        };
        match candidate {
            ReadCategory::RateLimit => return candidate,
            ReadCategory::Permission => category = candidate,
            ReadCategory::NotFound if category == ReadCategory::Unknown => category = candidate,
            _ => {}
        }
    }
    category
}

pub(crate) fn with_issue_context(
    mut error: ProjectReadError,
    item: &ProjectItem,
) -> ProjectReadError {
    error.operation = ReadOperation::DirectIssue;
    error.item_id = Some(item.item_id.clone());
    error.issue_id = Some(item.issue_node_id.clone());
    error
}

pub(crate) fn item_error(project_id: &str, item_id: &str, code: &str) -> ProjectReadError {
    ProjectReadError {
        operation: ReadOperation::ProjectPage,
        project_id: Some(project_id.to_owned()),
        item_id: Some(item_id.to_owned()),
        issue_id: None,
        category: ReadCategory::Malformed,
        status: None,
        code: code.to_owned(),
    }
}

pub(crate) fn required_string(value: &Value, key: &str, category: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| category.to_owned())
}
