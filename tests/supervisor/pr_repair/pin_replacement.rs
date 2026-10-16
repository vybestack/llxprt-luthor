use super::*;
use crate::pin_replacement_fixture as fixture;
use luthor::state::{launches, pin_replacement};
use luthor::worker_build::WorkerBuild;

#[test]
fn repin_pr_repair_dispatches_compatible_replacement_without_rewriting_old_plan() {
    let mut f = Fixture::new();
    let ready =
        journal::evidence_payloads(&f.store, "task", "attempt-real", "supervisor_ready").unwrap();
    let ready: serde_json::Value = serde_json::from_str(&ready[0]).unwrap();
    wait_for_process_and_group_absence(ready["pid"].as_i64().unwrap() as i32);
    let first = launches::launch_intent(&f.store, "attempt-real")
        .unwrap()
        .unwrap();
    let build = fixture::replacement(&mut f.config, &mut f.store);
    fs::write(
        &build.path,
        r#"#!/bin/sh
if test "$#" = 1 && test "$1" = --session-state-versions; then
  printf '{"session_state_versions":[3]}\n'
  exit 0
fi
exec /bin/sh "$0.behavior" "$@"
"#,
    )
    .unwrap();
    let build = WorkerBuild::capture(&build.path, Path::new("/")).unwrap();
    let audit = pin_replacement::replace(&mut f.store, &f.config, "task", fixture::request(&build))
        .unwrap();
    fs::remove_file(&audit.previous_worker_build.path).unwrap();
    let next = f.repair().unwrap().remove(0);
    assert_eq!(next.executable, build.path);
    assert_eq!(next.worker_build, Some(build));
    f.run(&next, "repaired");
    assert!(matches!(
        f.reconcile(&next.attempt_id),
        Reconciliation::Completed { .. }
    ));
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    assert_eq!(
        launches::launch_intent(&f.store, "attempt-real")
            .unwrap()
            .unwrap(),
        first
    );
}
