use super::*;
use luthor::state::{journal, task_records};

pub(crate) fn existing_target_matches_only_one_persisted_target_and_rejects_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    let mut selected = candidate("node-7", 7);
    selected.repository = "org/tracker".into();
    task_records::create_task(&mut store, "task-7", &selected, "rev", &config()).unwrap();

    assert!(task_records::existing_target(&store, "org/tracker", 7).unwrap());
    assert!(!task_records::existing_target(&store, "other/tracker", 7).unwrap());
    assert!(!task_records::existing_target(&store, "org/tracker", 8).unwrap());
    assert!(task_records::existing_target(&store, "", 7).is_err());
    assert!(task_records::existing_target(&store, "org/tracker", 0).is_err());
}

pub(crate) fn existing_target_rejects_ambiguous_persisted_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    let first = candidate("node-7", 7);
    let second = candidate("node-8", 7);
    task_records::create_task(&mut store, "task-7", &first, "rev", &config()).unwrap();
    task_records::create_task(&mut store, "task-8", &second, "rev", &config()).unwrap();
    assert!(matches!(
        task_records::existing_target(&store, "org/tracker", 7),
        Err(StateError::AmbiguousTarget(_, 7))
    ));
}

pub(crate) fn invalid_config_does_not_create_task_or_selection_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    let mut invalid = config();
    invalid.initial.args = vec!["--prompt".into(), "PRIVATE-TOKEN: DEMO_VALUE".into()];
    let error = task_records::create_task(&mut store, "t1", &candidate("i1", 1), "rev", &invalid)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "invalid configuration");
    assert!(!error.contains("DEMO_VALUE"));
    assert_eq!(task_records::task_count(&store).unwrap(), 0);
    assert_eq!(
        task_records::selection_evidence(&store, "t1").unwrap(),
        None
    );
}

pub(crate) fn duplicate_identity_does_not_leave_partial_task() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    task_records::create_task(&mut store, "t1", &candidate("ISSUE1", 1), "rev", &config()).unwrap();
    let _ = task_records::create_task(&mut store, "t2", &candidate("ISSUE1", 1), "rev", &config());
    assert_eq!(task_records::task_count(&store).unwrap(), 1);
}

pub(crate) fn invalid_selections_leave_no_task_or_evidence_after_reopen() {
    for name in [
        "unrelated mapping",
        "unrelated source",
        "mismatched marker",
        "unexpected milestone",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut candidate = candidate(name, 1);
        let mut config = config();
        match name {
            "unrelated mapping" => candidate.mapping.code_repository = "evil/repo".into(),
            "unrelated source" => candidate.source.project_id = "other-project".into(),
            "mismatched marker" => {
                candidate.marker = Marker::Label {
                    name: "other".into(),
                }
            }
            "unexpected milestone" => {
                config.sources[0].milestone = Some("v2".into());
                candidate.source = config.sources[0].clone();
                candidate.milestone_title = Some("unexpected".into());
            }
            _ => unreachable!(),
        }
        let mut store = StateStore::open(dir.path(), 1).unwrap();
        assert!(
            matches!(
                task_records::create_task(&mut store, "task", &candidate, "rev", &config),
                Err(StateError::InvalidSelection)
            ),
            "{name}"
        );
        drop(store);
        let reopened = StateStore::open(dir.path(), 1).unwrap();
        assert_eq!(task_records::task_count(&reopened).unwrap(), 0, "{name}");
        assert_eq!(
            task_records::selection_evidence(&reopened, "task").unwrap(),
            None,
            "{name}"
        );
    }
}

pub(crate) fn optional_source_persists_actual_issue_milestone_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut candidate = candidate("milestone-issue", 8);
    candidate.milestone_title = Some("0.12.0".into());
    candidate.milestone_id = Some("MILESTONE1".into());
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    task_records::create_task(&mut store, "task", &candidate, "rev", &config()).unwrap();
    drop(store);
    let reopened = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(task_records::task_count(&reopened).unwrap(), 1);
    let selection = task_records::selection_evidence(&reopened, "task")
        .unwrap()
        .unwrap();
    assert_eq!(selection.candidate, candidate);
    assert_eq!(selection.candidate.source.milestone, None);
    assert_eq!(
        selection.candidate.milestone_title.as_deref(),
        Some("0.12.0")
    );
    assert_eq!(
        selection.candidate.milestone_id.as_deref(),
        Some("MILESTONE1")
    );
    assert_eq!(
        journal::evidence_kinds(&reopened, "task").unwrap(),
        vec!["selection"]
    );
}

pub(crate) fn configured_milestone_selection_is_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let mut candidate = candidate("milestone-issue", 8);
    let mut config = config();
    config.sources[0].milestone = Some("v2".into());
    candidate.source = config.sources[0].clone();
    candidate.milestone_title = Some("v2".into());
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    task_records::create_task(&mut store, "task", &candidate, "rev", &config).unwrap();
    drop(store);
    let reopened = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(task_records::task_count(&reopened).unwrap(), 1);
    assert_eq!(
        task_records::selection_evidence(&reopened, "task")
            .unwrap()
            .unwrap()
            .candidate,
        candidate
    );
}
