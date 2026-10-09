use super::{correction_for_config, initial_branch_removal_plan, validate_removal_config};
use crate::{
    config::{Config, TaskValues},
    model::EffectiveConfigSnapshot,
    state::{context::FutureTemplateCorrection, continuation},
};
use rusqlite::{Connection, OpenFlags};
use std::{fs, path::Path};

#[test]
#[ignore = "requires saved live #12 state; immutable reads only, no authorization or launch"]
fn live_saved_issue12_dual_template_policy_proposal_read_only() {
    let root = Path::new("/tmp/lrs12331");
    let path = root.join("state.sqlite3");
    let config_path =
        "/Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1/tmp/luthor-rs-12-331-config.json";
    let db_before = fs::read(&path).unwrap();
    let config_before = fs::read(config_path).unwrap();
    let wal_before = fs::read(root.join("state.sqlite3-wal")).unwrap();
    let shm_before = fs::read(root.join("state.sqlite3-shm")).unwrap();
    assert!(wal_before.is_empty());
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
    let mut config: Config = serde_json::from_slice(&config_before).unwrap();
    assert_eq!(
        EffectiveConfigSnapshot::from(&config),
        context.selection().effective_config
    );
    let initial_index = super::branch_index(&config.initial.args, "luthor/{task.id}").unwrap();
    let resume_index = super::branch_index(&config.resume.args, "luthor/{task.id}").unwrap();
    config.initial.args.drain(initial_index..initial_index + 2);
    config.resume.args.drain(resume_index..resume_index + 2);
    validate_removal_config(&context, &config, "read-only-dual-template-proposal-v1").unwrap();
    let correction = correction_for_config(&context, &EffectiveConfigSnapshot::from(&config))
        .unwrap()
        .unwrap();
    let FutureTemplateCorrection::NativeInitialAndResumeBranchRemovalV1 { initial, resume } =
        correction;
    assert_eq!(initial.index, initial_index);
    assert_eq!(resume.index, resume_index);
    assert_eq!(initial.removed, ["--branch", "luthor/{task.id}"]);
    assert_eq!(resume.removed, initial.removed);
    let effective = initial_branch_removal_plan(&context).unwrap();
    let mut expected = context.plan().clone();
    let rendered_index = super::delta(&expected).unwrap().index;
    expected.args.drain(rendered_index..rendered_index + 2);
    assert_eq!(effective, expected);
    assert_eq!(effective.config_revision, context.plan().config_revision);
    assert_eq!(effective.args.last(), context.plan().args.last());
    assert!(
        effective
            .args
            .last()
            .unwrap()
            .contains("and Fixes #12 lines.")
    );
    assert_new_issue331_templates(&config);
    assert_eq!(
        continuation::read_context(&db, root, context.task_id(), context.attempt_id()).unwrap(),
        context
    );
    drop(db);
    assert_eq!(fs::read(path).unwrap(), db_before);
    assert_eq!(fs::read(config_path).unwrap(), config_before);
    assert_eq!(
        fs::read(root.join("state.sqlite3-wal")).unwrap(),
        wal_before
    );
    assert_eq!(
        fs::read(root.join("state.sqlite3-shm")).unwrap(),
        shm_before
    );
    println!(
        "Saved #12 dual-template policy proposal accepted. Initial plan removes one pair; original revision and prompt preserved. Future #331 template rendering accepted. No authorization, dispatch, native worker, resume or retry."
    );
}

fn assert_new_issue331_templates(config: &Config) {
    let values = TaskValues {
        task_issue_number: "331".into(),
        task_repository: "vybestack/llxprt-code-rs".into(),
        task_issue_url: "https://github.com/vybestack/llxprt-code-rs/issues/331".into(),
        task_id: "task-future331".into(),
        attempt_id: "attempt-future331".into(),
        worktree: config
            .worktree_root
            .join("task-future331")
            .to_string_lossy()
            .into_owned(),
    };
    for template in [&config.initial, &config.resume] {
        let rendered = template.render(&values).unwrap();
        assert_eq!(rendered.executable, template.executable);
        assert!(
            !rendered
                .args
                .iter()
                .any(|arg| arg == "--branch" || arg.starts_with("--branch="))
        );
        assert!(crate::launch_command::requires_pair(
            &rendered.args,
            "--session",
            &values.task_id
        ));
        assert!(crate::launch_command::requires_pair(
            &rendered.args,
            "--max-tool-calls",
            "512"
        ));
        assert!(crate::launch_command::requires_pair(
            &rendered.args,
            "--turn-time",
            "4h"
        ));
        assert!(rendered.args.last().unwrap().contains("Fixes #331"));
    }
    assert_ne!(config.initial.args.last(), config.resume.args.last());
}
