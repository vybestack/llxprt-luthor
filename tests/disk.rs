use luthor::{
    disk::{DiskSpace, inspect},
    state::StateError,
};
use std::{fs, os::unix::fs::symlink};

#[test]
fn disk_space_uses_worktree_filesystem_before_root_creation_and_holds_strictly_below_minimum() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("new/worktrees");
    let measured = inspect(&root, 0).unwrap();
    assert_eq!(measured.worktree_root, root);
    assert!(!measured.dispatch_held);
    assert!(!root.exists());
    for (available_bytes, held) in [(9, true), (10, false), (11, false)] {
        let space = DiskSpace {
            available_bytes,
            min_free_space_bytes: 10,
            dispatch_held: held,
            worktree_root: root.clone(),
        };
        assert_eq!(space.enforce().is_err(), held);
        if held {
            assert!(matches!(
                space.enforce(),
                Err(StateError::LowDiskSpace {
                    available: 9,
                    required: 10
                })
            ));
        }
    }
}

#[test]
fn disk_space_invalid_roots_fail_instead_of_dispatching_with_unknown_space() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("file"), "not a directory").unwrap();
    assert!(inspect(&dir.path().join("file"), 0).is_err());
    symlink(dir.path(), dir.path().join("alias")).unwrap();
    assert!(inspect(&dir.path().join("alias/worktrees"), 0).is_err());
    assert!(inspect(std::path::Path::new("relative"), 0).is_err());
}
