use serde_json::Value;

use super::{
    response::{classify_graphql_errors, item_error, required_string},
    types::{Page, ProjectItem, ProjectReadError, ReadCategory, ReadOperation},
};

struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Default)]
struct ProjectFields {
    supported: Vec<(String, String)>,
    unsupported: Vec<String>,
}

enum FieldValue {
    Supported(String, String),
    Unsupported(String),
    Ignored,
}

pub(crate) fn parse_page(
    value: &Value,
    project_id: &str,
) -> Result<Page<ProjectItem>, ProjectReadError> {
    if let Some(errors) = value.get("errors").and_then(Value::as_array) {
        return Err(ProjectReadError {
            operation: ReadOperation::ProjectPage,
            project_id: Some(project_id.to_owned()),
            item_id: None,
            issue_id: None,
            category: classify_graphql_errors(errors),
            status: None,
            code: "graphql-error".to_owned(),
        });
    }
    let connection = value
        .pointer("/data/node/items")
        .ok_or_else(|| "missing-project-connection".to_owned())?;
    let nodes = connection
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "invalid-project-items".to_owned())?;
    let page_info = parse_page_info(connection)?;
    let mut items = Vec::new();
    for node in nodes {
        if let Some(item) = parse_item(node, project_id)? {
            items.push(item);
        }
    }
    Ok(Page {
        items,
        has_next_page: page_info.has_next_page,
        end_cursor: page_info.end_cursor,
    })
}

fn parse_page_info(connection: &Value) -> Result<PageInfo, ProjectReadError> {
    let info = connection
        .get("pageInfo")
        .ok_or_else(|| "missing-page-info".to_owned())?;
    let has_next_page = info
        .get("hasNextPage")
        .and_then(Value::as_bool)
        .ok_or_else(|| "invalid-page-info".to_owned())?;
    let end_cursor = match info.get("endCursor") {
        Some(Value::String(cursor)) => Some(cursor.clone()),
        Some(Value::Null) => None,
        _ => return Err("invalid-page-info".to_owned().into()),
    };
    if has_next_page && end_cursor.as_deref().is_none_or(str::is_empty) {
        return Err("missing-page-cursor".to_owned().into());
    }
    Ok(PageInfo {
        has_next_page,
        end_cursor,
    })
}

fn parse_item(node: &Value, project_id: &str) -> Result<Option<ProjectItem>, ProjectReadError> {
    let item_id =
        required_string(node, "id", "invalid-project-item").map_err(|_| ProjectReadError {
            operation: ReadOperation::ProjectPage,
            project_id: Some(project_id.to_owned()),
            item_id: None,
            issue_id: None,
            category: ReadCategory::Malformed,
            status: None,
            code: "invalid-project-item".to_owned(),
        })?;
    let Some(content) = issue_content(node, project_id, &item_id)? else {
        return Ok(None);
    };
    let issue_node_id = required_string(content, "id", "invalid-project-issue")?;
    let issue_number = content
        .get("number")
        .and_then(Value::as_u64)
        .ok_or_else(|| "invalid-project-issue".to_owned())?;
    let repository = content
        .pointer("/repository/nameWithOwner")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "invalid-project-issue".to_owned())?;
    let tracker_repo_id = required_string(
        content
            .get("repository")
            .ok_or_else(|| "invalid-project-issue".to_owned())?,
        "id",
        "invalid-project-issue",
    )?;
    let fields = parse_fields(node)?;
    Ok(Some(ProjectItem {
        item_id,
        issue_node_id,
        repository: repository.to_owned(),
        tracker_repo_id,
        issue_number,
        fields: fields.supported,
        unsupported_fields: fields.unsupported,
    }))
}

fn issue_content<'a>(
    node: &'a Value,
    project_id: &str,
    item_id: &str,
) -> Result<Option<&'a Value>, ProjectReadError> {
    let content = node
        .get("content")
        .ok_or_else(|| item_error(project_id, item_id, "missing-project-item-content"))?;
    if content.is_null() {
        return Err(item_error(project_id, item_id, "null-project-item-content"));
    }
    let typename = content
        .get("__typename")
        .and_then(Value::as_str)
        .ok_or_else(|| item_error(project_id, item_id, "missing-project-item-content-type"))?;
    match typename {
        "PullRequest" => Ok(None),
        "Issue" => Ok(Some(content)),
        _ => Err(item_error(
            project_id,
            item_id,
            "unsupported-project-item-content-type",
        )),
    }
}

fn parse_fields(node: &Value) -> Result<ProjectFields, ProjectReadError> {
    let values = node
        .get("fieldValues")
        .ok_or_else(|| "missing-project-field-values".to_owned())?;
    if values
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err("incomplete-project-field-values".to_owned().into());
    }
    let nodes = values
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "invalid-project-field-values".to_owned())?;
    let mut fields = ProjectFields::default();
    for field in nodes {
        match parse_field(field)? {
            FieldValue::Supported(name, value) => fields.supported.push((name, value)),
            FieldValue::Unsupported(name) => fields.unsupported.push(name),
            FieldValue::Ignored => {}
        }
    }
    Ok(fields)
}

fn parse_field(field: &Value) -> Result<FieldValue, ProjectReadError> {
    match field.get("__typename").and_then(Value::as_str) {
        Some("ProjectV2ItemFieldSingleSelectValue") => Ok(FieldValue::Supported(
            field_name(field)?,
            required_string(field, "name", "invalid-project-field")?,
        )),
        Some("ProjectV2ItemFieldTextValue") => Ok(FieldValue::Supported(
            field_name(field)?,
            required_string(field, "text", "invalid-project-field")?,
        )),
        Some("ProjectV2ItemFieldDateValue" | "ProjectV2ItemFieldIterationValue") => {
            Ok(FieldValue::Unsupported(field_name(field)?))
        }
        Some(
            "ProjectV2ItemFieldNumberValue"
            | "ProjectV2ItemFieldUserValue"
            | "ProjectV2ItemFieldRepositoryValue"
            | "ProjectV2ItemFieldLabelValue"
            | "ProjectV2ItemFieldMilestoneValue"
            | "ProjectV2ItemFieldPullRequestValue",
        ) => Ok(FieldValue::Ignored),
        Some(_) | None => Err("unsupported-project-field-value-type".to_owned().into()),
    }
}

fn field_name(field: &Value) -> Result<String, ProjectReadError> {
    Ok(required_string(
        field
            .get("field")
            .ok_or_else(|| "invalid-project-field".to_owned())?,
        "name",
        "invalid-project-field",
    )?)
}
