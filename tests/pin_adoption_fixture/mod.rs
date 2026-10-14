//! Convert a fully accounted fixture to pre-pin history, including dispatch artifacts.
use luthor::{
    config::Config,
    state::{
        StateStore, launches,
        pin_adoption::{self, PinAdoptionRequest},
    },
    worker_build::WorkerBuild,
};
use serde_json::Value;
use std::fs;

pub(crate) fn legacy(config: &Config, store: &StateStore) -> WorkerBuild {
    let plan: luthor::supervisor::LaunchPlan = serde_json::from_str(
        &launches::launch_intent(store, "attempt-real")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.execute("UPDATE intents SET detail=json_remove(detail,'$.worker_build') WHERE task_id='task' AND kind IN ('launch','supervisor_dispatch')", []).unwrap();
    let path = config.state_root.join("attempts/attempt-real.plan.json");
    if path.exists() {
        let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        saved.as_object_mut().unwrap().remove("worker_build");
        fs::write(path, serde_json::to_vec(&saved).unwrap()).unwrap();
    }
    plan.worker_build.unwrap()
}

pub(crate) fn request(build: &WorkerBuild) -> PinAdoptionRequest<'_> {
    PinAdoptionRequest {
        executable: &build.path,
        sha256: &build.sha256,
        actor: "operator",
        reason: "unchanged executable for the task lifetime",
    }
}

pub(crate) fn adopt(store: &mut StateStore, build: &WorkerBuild) {
    let audit = pin_adoption::adopt(store, "task", request(build)).unwrap();
    assert_eq!(audit.worker_build, *build);
    assert_eq!(audit.actor, "operator");
    assert_eq!(audit.legacy_attempts, ["attempt-real"]);
    assert!(audit.observed_at_unix_secs > 0);
}

pub(crate) fn rows(config: &Config) -> Vec<String> {
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.prepare("SELECT kind||':'||detail FROM intents ORDER BY sequence")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
