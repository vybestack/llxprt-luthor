//! Read-only admission snapshot; this never grants permission to launch.
use super::{StateError, requests::RequestKind};
use crate::model::{LaunchPlan, SelectionEvidence};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

pub(crate) fn read(
    root: &Path,
    task: &str,
    kind: &RequestKind,
    previous_attempt_id: &str,
) -> Result<(LaunchPlan, SelectionEvidence), StateError> {
    let db =
        Connection::open_with_flags(root.join("state.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tx = db.unchecked_transaction()?;
    let previous = match kind {
        RequestKind::Retry { .. } => super::launches::retry_plan(&tx, task, previous_attempt_id)?,
        RequestKind::Resume => {
            let (_, latest, _) = super::launches::read_resume_context(&tx, task)?;
            serde_json::from_str(&latest)?
        }
    };
    let selection = super::attempts::attempt_selection(&tx, &previous)?;
    let claim: String = tx.query_row(
        "SELECT detail FROM intents WHERE task_id=?1 AND kind='claim_assignment'",
        [task],
        |row| row.get(0),
    )?;
    let expected = serde_json::json!({"principal":selection.effective_config.assignment_login,
        "repository":selection.candidate.repository,"number":selection.candidate.issue_number});
    if serde_json::from_str::<serde_json::Value>(&claim)? != expected {
        return Err(StateError::InvalidSelection);
    }
    tx.commit()?;
    Ok((previous, selection))
}
