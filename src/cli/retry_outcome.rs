//! Cached typed terminal outcome only; operator views never classify log text.
use super::CliError;
use crate::coordinator::OutcomeObservation;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

pub(crate) fn present(conn: &Connection, task: &str, output: &mut Value) -> Result<(), CliError> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload FROM evidence WHERE task_id=?1 AND kind='worker_outcome'
         AND attempt_id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1)",
            [task],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| CliError::Database)?;
    let observation = payload
        .map(|payload| serde_json::from_str::<OutcomeObservation>(&payload))
        .transpose()
        .map_err(|_| CliError::Database)?;
    output["worker_outcome"] = json!(observation.as_ref().map(|value| &value.outcome));
    output["worker_outcome_observed_at_unix_secs"] =
        json!(observation.map(|value| value.observed_at_unix_secs));
    Ok(())
}
