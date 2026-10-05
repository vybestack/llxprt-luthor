use super::{ContinuationResult, Lane, Refusal};
use luthor::{
    coordinator::{ContinuationDependencies, SupervisorLauncher, continue_never_dispatched},
    state::StateStore,
    supervisor::{self, LaunchPlan, SupervisorError},
};
use rusqlite::{Connection, params};
use std::path::PathBuf;

struct ProductionProbe {
    missing_binary: PathBuf,
}
impl SupervisorLauncher for ProductionProbe {
    fn launch(&mut self, store: &mut StateStore, plan: &LaunchPlan) -> Result<(), SupervisorError> {
        assert_eq!(
            store
                .evidence_payloads("task-a", "attempt-task-a", "never_dispatched_authorized")
                .unwrap()
                .len(),
            1
        );
        let result = supervisor::execute_with_binary(store, plan, &self.missing_binary);
        assert!(
            matches!(result, Err(SupervisorError::Io(_))),
            "saved JSON must reach the missing-binary spawn, not fail the dispatch identity gate"
        );
        result
    }
}

#[test]
fn continuation_production_dispatch_uses_original_sqlite_plan_bytes_and_holds_spawn_failure() {
    let mut lane = Lane::new();
    let saved = serde_json::to_string_pretty(&lane.plan).unwrap();
    let db = Connection::open(lane.f.store.root().join("state.sqlite3")).unwrap();
    db.execute("UPDATE intents SET detail=?1 WHERE kind='launch'", [&saved])
        .unwrap();
    let mut prs = std::mem::take(&mut lane.github.prs);
    let result = continue_never_dispatched(
        &mut lane.f.store,
        ContinuationDependencies {
            task_id: "task-a",
            attempt_id: "attempt-task-a",
            actor: "bot",
            config: &lane.f.config,
            config_revision: "revision",
            projects: &mut lane.github,
            prs: &mut prs,
            local: &mut lane.local,
            processes: &mut lane.processes,
            launcher: &mut ProductionProbe {
                missing_binary: lane.f.dir.path().join("nonexistent-supervisor"),
            },
        },
    )
    .unwrap();
    assert_eq!(result, ContinuationResult::Held(Refusal::LaunchFailed));
    let dispatched: String = db
        .query_row(
            "SELECT detail FROM intents WHERE kind='supervisor_dispatch' AND attempt_id=?1",
            params!["attempt-task-a"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dispatched, saved);
    assert_eq!(
        lane.f
            .store
            .launch_intent("attempt-task-a")
            .unwrap()
            .unwrap(),
        saved
    );
    let private: LaunchPlan = serde_json::from_slice(
        &std::fs::read(
            lane.f
                .store
                .root()
                .join("attempts/attempt-task-a.plan.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(private, lane.plan);
    assert_eq!(lane.f.store.reservation_count().unwrap(), 1);
    assert!(
        lane.f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .is_err()
    );
}
