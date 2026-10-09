use super::{error::CliError, logs, show, status};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

fn database(root: &Path) -> Result<Connection, CliError> {
    Connection::open_with_flags(root.join("state.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| CliError::Database)
}

pub fn execute(root: &Path, args: &[String]) -> Result<String, CliError> {
    let command = args.first().ok_or(CliError::Arguments)?;
    let conn = database(root)?;
    let output = match command.as_str() {
        "status" if args.len() == 1 => status::status(&conn, root)?,
        "show" if args.len() == 2 => show::show(&conn, root, &args[1])?,
        "logs" if args.len() == 2 || args.len() == 4 && args[2] == "--attempt" => {
            logs::logs(&conn, root, &args[1], args.get(3).map(String::as_str))?
        }
        _ => return Err(CliError::Arguments),
    };
    serde_json::to_string(&output).map_err(|_| CliError::Serialization)
}
