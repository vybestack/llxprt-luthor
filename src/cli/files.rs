use super::CliError;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

pub(crate) fn attempts_dir(root: &Path) -> Result<PathBuf, CliError> {
    let root = fs::canonicalize(root).map_err(|_| CliError::UnsafeLog)?;
    let dir = root.join("attempts");
    if fs::symlink_metadata(&dir)
        .map_err(|_| CliError::UnsafeLog)?
        .file_type()
        .is_symlink()
    {
        return Err(CliError::UnsafeLog);
    }
    let dir = fs::canonicalize(dir).map_err(|_| CliError::UnsafeLog)?;
    if dir.parent() != Some(root.as_path()) {
        return Err(CliError::UnsafeLog);
    }
    Ok(dir)
}

pub(crate) fn private_file(path: &Path) -> Result<(File, fs::Metadata), CliError> {
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let before = fs::symlink_metadata(path).map_err(|_| CliError::UnsafeLog)?;
    if !before.file_type().is_file() {
        return Err(CliError::UnsafeLog);
    }
    #[cfg(unix)]
    if before.permissions().mode() & 0o077 != 0 {
        return Err(CliError::UnsafeLog);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path).map_err(|_| CliError::UnsafeLog)?;
    let meta = file.metadata().map_err(|_| CliError::UnsafeLog)?;
    if !meta.is_file() {
        return Err(CliError::UnsafeLog);
    }
    #[cfg(unix)]
    if meta.dev() != before.dev()
        || meta.ino() != before.ino()
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(CliError::UnsafeLog);
    }
    Ok((file, meta))
}

pub(crate) fn log_paths(
    dir: &Path,
    attempt: &str,
    receipt: Option<&ExitReceipt>,
) -> Result<[PathBuf; 2], CliError> {
    let paths = [
        dir.join(format!("{attempt}.stdout.log")),
        dir.join(format!("{attempt}.stderr.log")),
    ];
    let recorded_paths_match = receipt.is_none_or(|r| {
        [&r.stdout_path, &r.stderr_path]
            .into_iter()
            .zip(&paths)
            .all(|(recorded, expected)| {
                fs::canonicalize(recorded).is_ok_and(|canonical| canonical == *expected)
            })
    });
    if !recorded_paths_match {
        return Err(CliError::UnsafeLog);
    }
    Ok(paths)
}

use crate::supervisor::ExitReceipt;
