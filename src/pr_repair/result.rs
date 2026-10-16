//! Read the retained worker report with no-follow, inode and size checks.
use super::PrRepairResult;
use std::{fs, io::Read, path::Path};

pub(crate) fn read(path: &Path) -> Result<PrRepairResult, std::io::Error> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let invalid = || std::io::Error::other("invalid PR repair report");
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > 4096 {
        return Err(invalid());
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file()
        || opened.dev() != meta.dev()
        || opened.ino() != meta.ino()
        || opened.len() > 4096
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
