//! Available (unprivileged-user) space on the filesystem hosting worktree_root.
use crate::{state::StateError, worktree};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiskSpace {
    pub worktree_root: PathBuf,
    pub available_bytes: u64,
    pub min_free_space_bytes: u64,
    pub dispatch_held: bool,
}

impl DiskSpace {
    pub fn enforce(&self) -> Result<(), StateError> {
        if self.available_bytes < self.min_free_space_bytes {
            return Err(StateError::LowDiskSpace {
                available: self.available_bytes,
                required: self.min_free_space_bytes,
            });
        }
        Ok(())
    }
}

pub fn inspect(root: &Path, minimum: u64) -> Result<DiskSpace, worktree::WorktreeError> {
    let root = worktree::check_root(root)?;
    // Dispatch has not created worktree_root yet. Its nearest existing ancestor
    // hosts the future directory; this is not a fallback for a failed space read.
    let existing =
        root.ancestors()
            .find(|path| path.exists())
            .ok_or(worktree::WorktreeError::Conflict(
                "worktree root has no existing ancestor",
            ))?;
    let available_bytes = fs2::available_space(existing)?;
    Ok(DiskSpace {
        worktree_root: root,
        available_bytes,
        min_free_space_bytes: minimum,
        dispatch_held: available_bytes < minimum,
    })
}
