//! Read-only presentation of the latest check-gate observation, not a fresh read.
use super::CliError;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

pub(crate) fn present(conn: &Connection, task: &str, output: &mut Value) -> Result<(), CliError> {
    let payload: Option<String> = conn.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND kind='pr_health' ORDER BY sequence DESC LIMIT 1",
        [task], |row| row.get(0),
    ).optional().map_err(|_| CliError::Database)?;
    let health: Option<crate::github::pull_request::PrHealth> = payload
        .map(|payload| serde_json::from_str(&payload))
        .transpose()
        .map_err(|_| CliError::Database)?;
    output["required_checks_source"] = health
        .as_ref()
        .map(|health| serde_json::to_value(health.required_checks_source))
        .transpose()
        .map_err(|_| CliError::Serialization)?
        .unwrap_or(Value::Null);
    output["last_pr_health"] = serde_json::to_value(health).map_err(|_| CliError::Serialization)?;
    Ok(())
}
