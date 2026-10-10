use super::{Case, Lane, Refusal};
use std::{
    fs,
    os::unix::fs::{MetadataExt, symlink},
    path::{Path, PathBuf},
};

fn artifact(root: &Path, kind: &str) -> PathBuf {
    let path = root.join("attempts/.attempt-task-a.receipt.tmp");
    match kind {
        "directory" => fs::create_dir(&path).unwrap(),
        "symlink" => symlink("missing-receipt", &path).unwrap(),
        "file" => fs::write(&path, "do not adopt").unwrap(),
        _ => unreachable!(),
    }
    path
}

fn unchanged(path: &Path, inode: u64, kind: &str) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert_eq!(metadata.ino(), inode);
    match kind {
        "directory" => assert!(metadata.is_dir()),
        "symlink" => assert_eq!(fs::read_link(path).unwrap(), Path::new("missing-receipt")),
        _ => assert_eq!(fs::read(path).unwrap(), b"do not adopt"),
    }
}

#[test]
fn safety_hidden_receipt_namespace_blocks_ordinary_and_amended_before_audit_or_dispatch() {
    for kind in ["file", "symlink", "directory"] {
        let mut lane = Lane::new();
        let path = artifact(lane.f.store.root(), kind);
        let inode = fs::symlink_metadata(&path).unwrap().ino();
        lane.held(Refusal::ArtifactConflict);
        unchanged(&path, inode, kind);
        let mut case = Case::new();
        let path = artifact(case.lane.f.store.root(), kind);
        let inode = fs::symlink_metadata(&path).unwrap().ino();
        case.held(Refusal::ArtifactConflict, 0);
        unchanged(&path, inode, kind);
    }
}
