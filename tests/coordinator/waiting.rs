//! Event-based waits for real coordinator fixture supervisors. Preserve the
//! existing fixture bounds and report explicit supervisor failures immediately.
use std::{
    fs, io,
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn artifact(path: &Path) -> Option<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => panic!("cannot read fixture artifact {}: {error}", path.display()),
    }
}

pub(crate) fn receipt(root: &Path, attempt: &str) {
    let attempts = root.join("attempts");
    let receipt = attempts.join(format!("{attempt}.receipt.json"));
    let error = attempts.join(format!("{attempt}.supervisor-error.json"));
    let log = attempts.join(format!("{attempt}.supervisor.log"));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            artifact(&error).is_none(),
            "fixture supervisor failed while waiting for {}: error={:?}; log={:?}",
            receipt.display(),
            artifact(&error),
            artifact(&log)
        );
        if artifact(&receipt).is_some() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fixture receipt not published: {}; error={:?}; log={:?}",
            receipt.display(),
            artifact(&error),
            artifact(&log)
        );
        thread::sleep(Duration::from_millis(20));
    }
}

// Receipt publication is not an owner-release barrier. Wait for teardown,
// never retry reconciliation or reinterpret a Held result as success.
pub(crate) fn owner_release(root: &Path, task: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match luthor::WorktreeOwner::acquire_existing(root, task) {
            Ok(owner) => {
                drop(owner);
                return;
            }
            Err(luthor::OwnershipError::Busy) => {
                assert!(
                    Instant::now() < deadline,
                    "fixture owner still busy: root={} task={task}",
                    root.display()
                );
                thread::sleep(Duration::from_millis(20));
            }
            Err(luthor::OwnershipError::Unavailable) => panic!(
                "fixture owner unavailable: root={} task={task}",
                root.display()
            ),
        }
    }
}
