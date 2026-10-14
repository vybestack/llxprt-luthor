//! Cached merge evidence only; status/show never authorize GitHub writes.
use super::CliError;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

pub(crate) fn present(conn: &Connection, task: &str, output: &mut Value) -> Result<(), CliError> {
    for (kind, field) in [
        ("tracker_closed", "tracker_closure"),
        ("pr_branch_updated", "last_branch_update"),
        ("pr_merged", "merge_evidence"),
        ("pr_merge_checks", "last_merge_checks"),
        ("pr_merge_error", "merge_error"),
    ] {
        let payload: Option<String> = conn.query_row(
            "SELECT payload FROM evidence WHERE task_id=?1 AND kind=?2 ORDER BY sequence DESC LIMIT 1",
            [task, kind], |row| row.get(0),
        ).optional().map_err(|_| CliError::Database)?;
        output[field] = payload
            .map(|payload| serde_json::from_str(&payload))
            .transpose()
            .map_err(|_| CliError::Database)?
            .unwrap_or(Value::Null);
    }
    Ok(())
}
