use super::support::Fixture;
use luthor::cli::CliError;
use std::fs;

pub(crate) fn logs_reject_foreign_attempt_traversal_and_world_readable_files() {
    let f = Fixture::new();
    f.task("one");
    f.task("two");
    f.attempt("two", "second", "session");
    f.logs("second");
    assert_eq!(
        f.run(&["logs", "one", "--attempt", "second"]),
        Err(CliError::AttemptNotFound)
    );
    for id in ["../second", "a/b", "..", "second\\other"] {
        assert_eq!(
            f.run(&["logs", "two", "--attempt", id]),
            Err(CliError::Arguments)
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            f.root().join("attempts/second.stdout.log"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert_eq!(f.run(&["logs", "two"]), Err(CliError::UnsafeLog));
    }
}

#[cfg(unix)]
pub(crate) fn logs_reject_symlinks_and_receipt_path_mismatch() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.task("task");
    f.attempt("task", "attempt", "session");
    let (stdout, stderr) = f.logs("attempt");
    fs::remove_file(&stdout).unwrap();
    symlink(&stderr, &stdout).unwrap();
    assert_eq!(f.run(&["logs", "task"]), Err(CliError::UnsafeLog));
    fs::remove_file(&stdout).unwrap();
    fs::write(&stdout, "hello").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&stdout, fs::Permissions::from_mode(0o600)).unwrap();
    let other = f.root().join("outside");
    fs::write(&other, "hello").unwrap();
    f.receipt("task", "attempt", other, stderr);
    assert_eq!(f.run(&["logs", "task"]), Err(CliError::UnsafeLog));
}
