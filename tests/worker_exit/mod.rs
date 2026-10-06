use luthor::{OwnershipError, WorktreeOwner, config::Config};
use std::{
    thread,
    time::{Duration, Instant},
};

pub(super) fn await_dispatched_worker_exit(config: &Config, task: &str, previous: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !config
        .state_root
        .join(format!("attempts/{previous}.receipt.json"))
        .exists()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let ownership_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match WorktreeOwner::acquire_existing(&config.state_root, task) {
            Ok(probe) => {
                drop(probe);
                break;
            }
            Err(OwnershipError::Busy) => {
                assert!(Instant::now() < ownership_deadline, "owner remained busy");
                thread::sleep(Duration::from_millis(20));
            }
            Err(OwnershipError::Unavailable) => panic!("worktree ownership is unavailable"),
        }
    }
}
