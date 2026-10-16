//! Last cycle quota observation, readable by status without making API calls.
use super::{StateError, StateStore};
use crate::github::budget::ApiUsage;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};

const PREFIX: &str = "github_budget_";

pub(crate) fn save(store: &StateStore, usage: &ApiUsage) -> Result<(), StateError> {
    let mut fields = serde_json::to_value(usage)?;
    fields["observed_at"] = json!(now()?);
    let tx = store.connection.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM state_meta WHERE key LIKE 'github_budget_%'",
        [],
    )?;
    for (key, value) in fields.as_object().expect("usage record") {
        if let Some(value) = value.as_u64() {
            tx.execute(
                "INSERT INTO state_meta(key,value) VALUES(?1,?2)",
                params![format!("{PREFIX}{key}"), value],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn load(conn: &Connection) -> Result<Value, StateError> {
    let mut stmt =
        conn.prepare("SELECT key,value FROM state_meta WHERE key LIKE 'github_budget_%'")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
    })?;
    let mut fields = Map::new();
    for row in rows {
        let (key, value) = row?;
        let name = key
            .strip_prefix(PREFIX)
            .ok_or(StateError::InvalidSelection)?;
        fields.insert(name.to_owned(), json!(value));
    }
    if fields.is_empty() {
        return Ok(Value::Null);
    }
    let usage: ApiUsage = serde_json::from_value(json!(fields))?;
    let exhausted = usage.exhausted(now()?);
    fields.insert("github_budget_exhausted".into(), json!(exhausted));
    Ok(json!(fields))
}

pub(crate) fn now() -> Result<u64, std::io::Error> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| std::io::Error::other("clock unavailable"))
}
