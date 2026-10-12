use crate::*;

fn remote_tip(f: &Fixture) -> String {
    git(
        &f.dir.path().join("remote.git"),
        &["rev-parse", "refs/heads/main"],
    )
}

fn advance_remote(f: &Fixture) -> String {
    let remote = f.dir.path().join("remote.git");
    let writer = f.dir.path().join("remote-writer");
    git(
        f.dir.path(),
        &["clone", remote.to_str().unwrap(), writer.to_str().unwrap()],
    );
    git(&writer, &["config", "user.name", "Fixture"]);
    git(&writer, &["config", "user.email", "fixture@example.org"]);
    git(
        &writer,
        &["commit", "--allow-empty", "-m", "merged upstream work"],
    );
    git(&writer, &["push", "origin", "main"]);
    remote_tip(f)
}

#[test]
fn fresh_remote_tip_is_saved_and_local_base_is_untouched() {
    let mut f = Fixture::new();
    let checkout = f.config.mappings[0].checkout.clone();
    let stale = git(&checkout, &["rev-parse", "main"]);
    let tip = advance_remote(&f);
    assert_ne!(tip, stale);
    f.task("task-fresh", 42, true);
    let WorktreeResult::Created(identity) = f.create("task-fresh").unwrap() else {
        panic!()
    };
    assert_eq!(identity.head, tip);
    assert_eq!(identity.base_commit.as_deref(), Some(tip.as_str()));
    assert_eq!(git(&checkout, &["rev-parse", "main"]), stale);
    let record = worktree_records::worktree_record(&f.store, "task-fresh")
        .unwrap()
        .unwrap();
    assert_eq!(record.intent.base, "main");
    assert_eq!(record.intent.base_commit, identity.base_commit);
    assert_eq!(record.identity, Some(identity.clone()));
    fs::remove_dir_all(f.dir.path().join("remote.git")).unwrap();
    drop(f.store);
    let mut store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        ensure_worktree(
            &mut store,
            "task-fresh",
            &f.config.worktree_root,
            &f.config.mappings[0]
        )
        .unwrap(),
        WorktreeResult::Existing(identity)
    );
}

#[test]
fn remote_base_does_not_require_a_local_base_branch() {
    let mut f = Fixture::new();
    let checkout = f.config.mappings[0].checkout.clone();
    let tip = remote_tip(&f);
    git(&checkout, &["switch", "--detach"]);
    git(&checkout, &["branch", "-D", "main"]);
    f.task("task-no-local-base", 42, true);
    luthor::worktree::preflight(
        "task-no-local-base",
        &f.config.worktree_root,
        &f.config.mappings[0],
    )
    .unwrap();
    let WorktreeResult::Created(identity) = f.create("task-no-local-base").unwrap() else {
        panic!()
    };
    assert_eq!(identity.head, tip);
    assert_eq!(git(&checkout, &["branch", "--list", "main"]), "");
}

#[test]
fn creation_uses_pinned_commit_even_if_git_refs_change_after_fetch() {
    let mut f = Fixture::new();
    let checkout = f.config.mappings[0].checkout.clone();
    let stale = git(&checkout, &["rev-parse", "main"]);
    let tip = advance_remote(&f);
    f.task("task-pinned", 42, true);
    let result = ensure_worktree_with_hooks(
        &mut f.store,
        "task-pinned",
        &f.config.worktree_root,
        &f.config.mappings[0],
        |_| {},
        || {
            git(
                &checkout,
                &["update-ref", "refs/luthor/task-bases/task-pinned", &stale],
            );
            fs::write(checkout.join(".git/FETCH_HEAD"), format!("{stale}\n")).unwrap();
        },
    )
    .unwrap();
    let WorktreeResult::Created(identity) = result else {
        panic!()
    };
    assert_eq!(identity.head, tip);
    assert_eq!(identity.base_commit, Some(tip));
}

#[test]
fn named_remote_and_explicit_url_fetch_the_configured_source_not_origin() {
    for push_remote in ["upstream", "git@github.com:org/fork.git"] {
        let mut f = Fixture::new();
        let checkout = f.config.mappings[0].checkout.clone();
        let tip = advance_remote(&f);
        git(
            &checkout,
            &["remote", "add", "upstream", "git@github.com:org/fork.git"],
        );
        let script = f.dir.path().join("fork-only-ssh");
        fs::write(&script, format!("#!/bin/sh\ncase \"$*\" in *org/fork.git*) exec git-upload-pack '{}' ;; *) exit 1 ;; esac\n", f.dir.path().join("remote.git").display())).unwrap();
        git(
            &checkout,
            &[
                "config",
                "core.sshCommand",
                &format!("sh '{}'", script.display()),
            ],
        );
        f.config.mappings[0].push_remote = push_remote.into();
        f.config.mappings[0].allowed_pr_head_repository = "org/fork".into();
        f.task("task-source", 42, true);
        let WorktreeResult::Created(identity) = f.create("task-source").unwrap() else {
            panic!()
        };
        assert_eq!(identity.head, tip);
        assert_eq!(identity.remote, "git@github.com:org/fork.git");
    }
}

#[test]
fn failed_or_missing_remote_base_holds_without_using_stale_refs() {
    for missing_branch in [false, true] {
        let mut f = Fixture::new();
        let checkout = f.config.mappings[0].checkout.clone();
        let stale = git(&checkout, &["rev-parse", "main"]);
        git(
            &checkout,
            &["update-ref", "refs/luthor/task-bases/task-failure", &stale],
        );
        fs::write(checkout.join(".git/FETCH_HEAD"), format!("{stale}\n")).unwrap();
        let remote = f.dir.path().join("remote.git");
        if missing_branch {
            git(&remote, &["update-ref", "-d", "refs/heads/main"]);
        } else {
            fs::remove_dir_all(remote).unwrap();
        }
        f.task("task-failure", 42, true);
        assert!(matches!(
            f.create("task-failure"),
            Err(WorktreeError::BaseFetch)
        ));
        assert_eq!(
            task_records::task_phase(&f.store, "task-failure")
                .unwrap()
                .as_deref(),
            Some("held")
        );
        let db = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
        let reason: String = db.query_row(
            "SELECT payload FROM evidence WHERE task_id=?1 AND kind='held_reason' ORDER BY sequence DESC LIMIT 1",
            ["task-failure"], |row| row.get(0)).unwrap();
        assert_eq!(reason, "remote base fetch failed");
        assert!(
            worktree_records::worktree_record(&f.store, "task-failure")
                .unwrap()
                .is_none()
        );
        assert!(!f.config.worktree_root.exists());
        assert_eq!(
            git(&checkout, &["branch", "--list", "luthor/task-failure"]),
            ""
        );
        assert_eq!(git(&checkout, &["rev-parse", "main"]), stale);
    }
}

#[test]
fn legacy_worktree_records_reopen_without_fetching_or_changing_serialized_evidence() {
    let mut f = Fixture::new();
    f.task("task-legacy", 42, true);
    let WorktreeResult::Created(mut identity) = f.create("task-legacy").unwrap() else {
        panic!()
    };
    let record = worktree_records::worktree_record(&f.store, "task-legacy")
        .unwrap()
        .unwrap();
    let mut intent_json = serde_json::to_value(record.intent).unwrap();
    let mut identity_json = serde_json::to_value(&identity).unwrap();
    intent_json.as_object_mut().unwrap().remove("base_commit");
    identity_json.as_object_mut().unwrap().remove("base_commit");
    let db = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
    db.execute(
        "UPDATE intents SET detail=?1 WHERE task_id='task-legacy' AND kind='worktree_create'",
        [intent_json.to_string()],
    )
    .unwrap();
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE task_id='task-legacy' AND kind='worktree_created'",
        [identity_json.to_string()],
    )
    .unwrap();
    fs::remove_dir_all(f.dir.path().join("remote.git")).unwrap();
    identity.base_commit = None;
    assert_eq!(
        f.create("task-legacy").unwrap(),
        WorktreeResult::Existing(identity.clone())
    );
    verify_snapshot(&identity).unwrap();
    let saved = worktree_records::worktree_record(&f.store, "task-legacy")
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(saved.intent).unwrap(), intent_json);
    assert_eq!(
        serde_json::to_value(saved.identity.unwrap()).unwrap(),
        identity_json
    );
}

#[test]
fn each_new_task_fetches_again_and_uses_the_mapped_base_branch() {
    let mut f = Fixture::new();
    let remote = f.dir.path().join("remote.git");
    let old = remote_tip(&f);
    f.task("task-old", 41, true);
    let WorktreeResult::Created(first) = f.create("task-old").unwrap() else {
        panic!()
    };
    assert_eq!(first.head, old);
    let tip = advance_remote(&f);
    git(
        &remote,
        &["update-ref", "refs/heads/work/current-base", &tip],
    );
    git(&remote, &["update-ref", "refs/heads/main", &old]);
    f.config.mappings[0].base_branch = "work/current-base".into();
    f.task("task-new", 42, true);
    let WorktreeResult::Created(second) = f.create("task-new").unwrap() else {
        panic!()
    };
    assert_eq!(second.base, "work/current-base");
    assert_eq!(second.head, tip);
    assert_eq!(second.base_commit, Some(tip));
    assert_eq!(git(&first.path, &["rev-parse", "HEAD"]), old);
    f.config.mappings[0].base_branch = "main".into();
    assert_eq!(
        f.create("task-old").unwrap(),
        WorktreeResult::Existing(first)
    );
}

#[test]
fn invalid_base_ref_is_held_without_interpreting_it_as_a_fetch_refspec() {
    let mut f = Fixture::new();
    f.config.mappings[0].base_branch = "main:refs/heads/other".into();
    f.task("task-invalid-base", 42, true);
    assert!(matches!(
        f.create("task-invalid-base"),
        Err(WorktreeError::Conflict("invalid base branch ref"))
    ));
    assert!(!f.config.worktree_root.exists());
    assert!(
        worktree_records::worktree_record(&f.store, "task-invalid-base")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        git(
            &f.config.mappings[0].checkout,
            &["branch", "--list", "other"]
        ),
        ""
    );
}
