//! Build identity is immutable session provenance, not a mutable template path.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerBuild {
    /// Resolved absolute path, including PATH/symlink resolution at selection.
    pub path: PathBuf,
    pub sha256: String,
}

impl WorkerBuild {
    pub fn capture(executable: &Path, cwd: &Path) -> io::Result<Self> {
        let path = resolve(executable, cwd)?;
        let mut file = fs::File::open(&path)?;
        let before = file.metadata()?;
        if !before.is_file() {
            return Err(io::Error::other("worker executable is not a regular file"));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        let after = file.metadata()?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err(io::Error::other("worker executable changed while hashing"));
        }
        Ok(Self {
            path,
            sha256: format!("{:x}", hash.finalize()),
        })
    }

    pub(crate) fn verify(&self) -> io::Result<()> {
        if !self.path.is_absolute()
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || Self::capture(&self.path, Path::new("/"))? != *self
        {
            return Err(io::Error::other("pinned worker build no longer matches"));
        }
        Ok(())
    }

    pub(crate) fn label(&self) -> String {
        format!("{} sha256={}", self.path.display(), self.sha256)
    }
}

fn resolve(executable: &Path, cwd: &Path) -> io::Result<PathBuf> {
    let paths = if executable.is_absolute() {
        vec![executable.to_owned()]
    } else if executable.components().count() > 1 {
        vec![cwd.join(executable)]
    } else {
        let path = env::var_os("PATH").ok_or_else(|| io::Error::other("PATH unavailable"))?;
        env::split_paths(&path)
            .map(|dir| {
                if dir.is_absolute() {
                    dir.join(executable)
                } else {
                    cwd.join(dir).join(executable)
                }
            })
            .collect()
    };
    for path in paths {
        if executable_file(&path)? {
            return fs::canonicalize(path);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "worker executable unavailable",
    ))
}

#[cfg(unix)]
fn executable_file(path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    match fs::metadata(path) {
        Ok(meta) => Ok(meta.is_file() && meta.permissions().mode() & 0o111 != 0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn executable_file(_: &Path) -> io::Result<bool> {
    Err(io::Error::other("worker execution unsupported"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::WorkerBuild;
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn resolves_relative_symlink_and_hashes_bytes_not_path_names() {
        let dir = tempfile::tempdir().unwrap();
        let worker = dir.path().join("worker");
        let bytes = b"#!/bin/sh\nexit 0\n";
        fs::write(&worker, bytes).unwrap();
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
        symlink("worker", dir.path().join("alias")).unwrap();
        let build = WorkerBuild::capture(std::path::Path::new("./alias"), dir.path()).unwrap();
        assert_eq!(build.path, fs::canonicalize(&worker).unwrap());
        assert_eq!(build.sha256, format!("{:x}", Sha256::digest(bytes)));
        build.verify().unwrap();
        fs::write(&worker, b"#!/bin/sh\nexit 1\n").unwrap();
        assert!(build.verify().is_err());
    }

    #[test]
    fn unavailable_and_non_executable_workers_are_not_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let worker = dir.path().join("worker");
        assert!(WorkerBuild::capture(&worker, dir.path()).is_err());
        fs::write(&worker, "not executable").unwrap();
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(WorkerBuild::capture(&worker, dir.path()).is_err());
    }
}
