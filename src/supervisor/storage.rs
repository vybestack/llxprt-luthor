use super::{error::SupervisorError, processes::valid_attempt};
use serde::Serialize;
use std::{fs::OpenOptions, io::Write};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

pub(crate) fn private_attempts(root: &Path) -> Result<PathBuf, SupervisorError> {
    let path = root.join("attempts");
    let metadata = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new().mode(0o700).create(&path)?;
            }
            #[cfg(not(unix))]
            fs::create_dir(&path)?;
            File::open(root)?.sync_all()?;
            fs::symlink_metadata(&path)?
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) => metadata,
    };
    if !metadata.file_type().is_dir() {
        return Err(SupervisorError::Conflict);
    }
    #[cfg(unix)]
    validate_attempt_privacy(&metadata, unsafe { libc::geteuid() })?;
    Ok(path)
}

#[cfg(unix)]
pub(crate) fn validate_attempt_privacy(
    metadata: &fs::Metadata,
    expected_owner: u32,
) -> Result<(), SupervisorError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if metadata.uid() != expected_owner || metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(SupervisorError::Conflict);
    }
    Ok(())
}

/// Verify or create private attempt storage before any externally visible claim.
pub fn preflight_attempt_storage(root: &Path) -> Result<(), SupervisorError> {
    private_attempts(root).map(|_| ())
}

/// Read-only version of private_attempts policy: missing storage is safe for
/// the ordinary launcher to create. Existing storage is never chmod'ed/adopted.
/// Returns true for any target-attempt artifact, including broken symlinks.
pub fn inspect_never_dispatched_storage(
    root: &Path,
    attempt: &str,
) -> Result<bool, SupervisorError> {
    if !valid_attempt(attempt) {
        return Err(SupervisorError::Conflict);
    }
    let attempts = root.join("attempts");
    let metadata = match fs::symlink_metadata(&attempts) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
        Ok(metadata) => metadata,
    };
    if !metadata.file_type().is_dir() {
        return Err(SupervisorError::Conflict);
    }
    #[cfg(unix)]
    validate_attempt_privacy(&metadata, unsafe { libc::geteuid() })?;
    for entry in fs::read_dir(attempts)? {
        let entry = entry?;
        // Prefix matching includes temporary names, unknown suffixes, sockets,
        // nested namespaces, and non-UTF8 suffixes. Metadata never follows links.
        if attempt_artifact_name(&entry.file_name(), attempt) {
            fs::symlink_metadata(entry.path())?;
            return Ok(true);
        }
    }
    Ok(false)
}

fn attempt_artifact_name(name: &std::ffi::OsStr, attempt: &str) -> bool {
    let bytes = name.as_encoded_bytes();
    bytes.starts_with(attempt.as_bytes())
        || bytes.starts_with(format!(".{attempt}.receipt.tmp").as_bytes())
}

#[cfg(all(test, unix))]
mod attempt_storage_tests {
    use super::*;

    #[test]
    fn hidden_receipt_artifact_matching_preserves_non_utf8_suffixes() {
        use std::os::unix::ffi::OsStringExt;
        for prefix in [
            b"attempt-a.unknown".as_slice(),
            b".attempt-a.receipt.tmp".as_slice(),
        ] {
            let mut bytes = prefix.to_vec();
            bytes.push(0xff);
            let name = std::ffi::OsString::from_vec(bytes);
            assert!(attempt_artifact_name(&name, "attempt-a"));
            assert!(!attempt_artifact_name(&name, "attempt-b"));
        }
        assert!(attempt_artifact_name(
            std::ffi::OsStr::new(".attempt-a.receipt.tmp"),
            "attempt-a"
        ));
        assert!(!attempt_artifact_name(
            std::ffi::OsStr::new(".attempt-b.receipt.tmp"),
            "attempt-a"
        ));
    }

    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    #[test]
    fn attempt_storage_privacy_requires_expected_owner_and_private_mode() {
        let root = tempfile::tempdir().unwrap();
        let attempts = root.path().join("attempts");
        fs::DirBuilder::new().mode(0o700).create(&attempts).unwrap();
        let metadata = fs::symlink_metadata(&attempts).unwrap();
        validate_attempt_privacy(&metadata, metadata.uid()).unwrap();
        assert!(matches!(
            validate_attempt_privacy(&metadata, metadata.uid().wrapping_add(1)),
            Err(SupervisorError::Conflict)
        ));
        for mode in [0o000, 0o600, 0o711, 0o750, 0o755, 0o770, 0o777] {
            fs::set_permissions(&attempts, fs::Permissions::from_mode(mode)).unwrap();
            let metadata = fs::symlink_metadata(&attempts).unwrap();
            assert!(matches!(
                validate_attempt_privacy(&metadata, metadata.uid()),
                Err(SupervisorError::Conflict)
            ));
            assert_eq!(metadata.permissions().mode() & 0o777, mode);
        }
    }
}

pub(crate) fn private_file(path: &Path) -> Result<File, SupervisorError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

#[cfg(unix)]
pub(crate) fn stop_socket(root: &Path, attempt: &str) -> PathBuf {
    root.join(format!("{attempt}.stop.sock"))
}

#[cfg(unix)]
pub fn validate_stop_socket_path(
    state_root: &Path,
    attempt_id: &str,
) -> Result<(), SupervisorError> {
    if !valid_attempt(attempt_id) {
        return Err(SupervisorError::Conflict);
    }
    use std::os::unix::ffi::OsStrExt;
    let path = stop_socket(&state_root.join("attempts"), attempt_id);
    let usable = unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
        .sun_path
        .len()
        - 1;
    if path.as_os_str().as_bytes().len() > usable {
        return Err(SupervisorError::StopSocketPathTooLong);
    }
    Ok(())
}

pub(crate) fn write_private_json<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), SupervisorError> {
    let mut file = private_file(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    File::open(path.parent().ok_or(SupervisorError::Conflict)?)?.sync_all()?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn write_private_json_atomic<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), SupervisorError> {
    let temp = path.with_extension("child.tmp");
    let mut file = private_file(&temp)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    File::open(path.parent().ok_or(SupervisorError::Conflict)?)?.sync_all()?;
    Ok(())
}
