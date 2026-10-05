use super::*;

pub(crate) fn expected_for_task_uses_selection_mapping_and_verified_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let (mut config, mut candidate) = configured(dir.path());
    config.mappings[0].allowed_pr_head_repository = "org/head".into();
    candidate.mapping = config.mappings[0].clone();
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    prepare_initial(&mut store, "task", "attempt-1").unwrap();
    let mut reader = ExpectedIdentityReader {
        login: "operator".into(),
        fail_target: false,
        target_id: 10,
        head_id: 20,
    };
    let expected = expected_for_task(&store, "task", &mut reader, "operator").unwrap();
    assert_eq!(
        (expected.repository_id, expected.head_repository_id),
        (10, 20)
    );
    assert_eq!(expected.task_branch, "luthor/task");
}

pub(crate) fn expected_for_task_rejects_changed_login_and_repository_id_lookup_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    prepare_initial(&mut store, "task", "attempt-1").unwrap();
    let mut reader = ExpectedIdentityReader {
        login: "attacker".into(),
        fail_target: false,
        target_id: 10,
        head_id: 20,
    };
    assert!(matches!(
        expected_for_task(&store, "task", &mut reader, "attacker"),
        Err(ExpectedPrError::AuthorMismatch)
    ));
    let mut reader = ExpectedIdentityReader {
        login: "operator".into(),
        fail_target: true,
        target_id: 10,
        head_id: 20,
    };
    assert!(matches!(
        expected_for_task(&store, "task", &mut reader, "operator"),
        Err(ExpectedPrError::RepositoryLookup(_))
    ));
}
