use super::*;
pub(super) mod retry_cases;
pub(super) mod stop_views;

pub(super) fn wait_for_worktree_owner_release(root: &Path, task: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match luthor::WorktreeOwner::acquire_existing(root, task) {
            Ok(owner) => {
                drop(owner);
                return;
            }
            Err(luthor::OwnershipError::Busy) => {
                assert!(Instant::now() < deadline, "worktree owner remained busy");
                thread::sleep(Duration::from_millis(10));
            }
            Err(luthor::OwnershipError::Unavailable) => {
                panic!("worktree owner availability could not be established")
            }
        }
    }
}
