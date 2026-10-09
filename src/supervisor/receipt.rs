use super::{binding, error::SupervisorError, storage::private_file};
use crate::model::{ExitReceipt, LaunchPlan};
use rusqlite::{Connection, OpenFlags};
use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};
#[cfg(unix)]
pub(crate) fn finish_receipt(
    root: &Path,
    plan: &LaunchPlan,
    receipt: &ExitReceipt,
    managed: bool,
) -> Result<(), SupervisorError> {
    if managed {
        let state_root = root.parent().ok_or(SupervisorError::Conflict)?;
        let db = Connection::open_with_flags(
            state_root.join("state.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        binding::verify_observed_plan(&db, state_root, plan)?;
    }
    write_receipt(root, &plan.attempt_id, receipt)
}

fn write_receipt(root: &Path, attempt: &str, receipt: &ExitReceipt) -> Result<(), SupervisorError> {
    let final_path = root.join(format!("{attempt}.receipt.json"));
    let temp_path = root.join(format!(".{attempt}.receipt.tmp"));
    let mut file = private_file(&temp_path)?;
    serde_json::to_writer(&mut file, receipt)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temp_path, &final_path)?;
    File::open(root)?.sync_all()?;
    Ok(())
}
