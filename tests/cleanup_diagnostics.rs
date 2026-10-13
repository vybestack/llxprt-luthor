use luthor::{
    cleanup::CleanupError, state::StateError, supervisor::SupervisorError, worktree::WorktreeError,
};

#[test]
fn cleanup_diagnostics_preserve_fixed_conflicts_without_echoing_paths_or_payloads() {
    let secret = "private-token-and-path";
    let errors = [
        (
            CleanupError::Conflict("cleanup owner unavailable"),
            "cleanup owner unavailable",
        ),
        (
            CleanupError::Worktree(WorktreeError::Conflict(
                "worktree repository, path or branch changed",
            )),
            "worktree repository, path or branch changed",
        ),
        (
            CleanupError::Io(std::io::Error::other(secret)),
            "cleanup IO failed (Other)",
        ),
        (
            CleanupError::State(StateError::Io(std::io::Error::other(secret))),
            "cleanup state IO failed",
        ),
        (
            CleanupError::State(StateError::DuplicateTask(secret.into(), secret.into())),
            "cleanup task identity duplicated",
        ),
        (
            CleanupError::State(StateError::Serialization(
                serde_json::from_str::<u64>(&format!("\"{secret}\"")).unwrap_err(),
            )),
            "cleanup state serialization failed",
        ),
        (
            CleanupError::Supervisor(SupervisorError::Io(std::io::Error::other(secret))),
            "cleanup supervisor IO failed (Other)",
        ),
    ];
    for (error, expected) in errors {
        let reason = error.operator_reason();
        assert_eq!(reason, expected);
        assert!(!reason.contains(secret));
    }
}
