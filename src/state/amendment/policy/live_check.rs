use super::{initial_branch_removal_plan, validate_removal_config};
use crate::{config::Config, state::continuation};
use rusqlite::{Connection, OpenFlags};

/// Explicitly opted-in external check. Immutable SQLite reads cannot alter WAL/SHM.
#[test]
#[ignore = "requires the saved live #12 database; reads only, never authorizes or launches"]
fn live_saved_issue12_policy_proposal_read_only() {
    let root = std::path::Path::new("/tmp/lrs12331");
    let path = root.join("state.sqlite3");
    assert_eq!(
        std::fs::metadata(root.join("state.sqlite3-wal"))
            .unwrap()
            .len(),
        0
    );
    let before = std::fs::read(&path).unwrap();
    let db = Connection::open_with_flags(
        format!("file:{}?immutable=1", path.display()),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    db.execute_batch("PRAGMA query_only=ON").unwrap();
    let context = continuation::read_context(
        &db,
        root,
        "task-16d0baadbd19711e525dcbcd6c7e9135",
        "attempt-d3c12e5590f529890229b0e10ed5c8bc",
    )
    .unwrap();
    assert_eq!(context.selection().candidate.issue_number, 12);
    let saved = context.saved_launch_plan().to_owned();
    let mut config: Config = serde_json::from_slice(
        &std::fs::read("/Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1/tmp/luthor-rs-12-331-config.json").unwrap(),
    ).unwrap();
    assert_eq!(
        crate::model::EffectiveConfigSnapshot::from(&config),
        context.selection().effective_config
    );
    let index = config
        .initial
        .args
        .iter()
        .position(|arg| arg == "--branch")
        .unwrap();
    assert_eq!(config.initial.args[index + 1], "luthor/{task.id}");
    config.initial.args.drain(index..index + 2);
    validate_removal_config(&context, &config, "read-only-initial-template-proposal").unwrap();
    let effective = initial_branch_removal_plan(&context).unwrap();
    let mut expected = context.plan().clone();
    expected.args.drain(index..index + 2);
    assert_eq!(effective, expected);
    assert_eq!(effective.args.last(), context.plan().args.last());
    assert!(
        effective
            .args
            .last()
            .unwrap()
            .contains("and Fixes #12 lines.")
    );
    let after =
        continuation::read_context(&db, root, context.task_id(), context.attempt_id()).unwrap();
    assert_eq!(after.saved_launch_plan(), saved);
    assert_eq!(after, context);
    drop(db);
    assert_eq!(std::fs::read(path).unwrap(), before);
    println!("Saved #12 exact policy proposal accepted; no authorization, dispatch or launch.");
}
