use luthor::state::{launches, scheduling, task_records};
#[cfg(unix)]
mod gated;
#[cfg(unix)]
mod hidden_receipt;
mod local;
mod refusals;
use super::{Lane, Refusal, amendment, reads};
use luthor::{
    coordinator::{
        AmendedContinuationDependencies, AmendedSupervisorLauncher, ContinuationResult,
        amend_never_dispatched_initial_branch,
    },
    github::pull_request::{LookupError, PullRequestReader},
    state::{NeverDispatchedContext, StateStore},
    supervisor::{LaunchPlan, SupervisorError},
};
use serde_json::Value;

#[derive(Default)]
struct Launcher {
    plans: Vec<LaunchPlan>,
    fail: bool,
}
impl AmendedSupervisorLauncher for Launcher {
    fn launch_amended(
        &mut self,
        store: &mut StateStore,
        context: &NeverDispatchedContext,
        config: &luthor::config::Config,
        revision: &str,
        _owner: &luthor::WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        assert_eq!(scheduling::reservation_count(store).unwrap(), 1);
        assert_eq!(
            context.amendment().unwrap().current_config_revision,
            revision
        );
        assert_eq!(context.plan().config_revision, "revision");
        let proof = store.begin_amended_supervision(
            context,
            config,
            revision,
            &_owner
                .protocol_evidence(store.root(), context.task_id(), context.attempt_id())
                .map_err(|_| SupervisorError::Conflict)?,
        )?;
        assert_eq!(proof.effective_plan, *context.effective_plan());
        assert_eq!(count(store, "evidence", "initial_branch_removed"), 1);
        self.plans.push(proof.effective_plan);
        if self.fail {
            Err(SupervisorError::ExecutionUnavailable)
        } else {
            Ok(())
        }
    }
}

struct Prs<'a> {
    inner: reads::Prs<'a>,
    calls: usize,
    identity_drift: bool,
}
impl PullRequestReader for Prs<'_> {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        self.calls += 1;
        if self.identity_drift && self.calls == 2 {
            return Ok("other".into());
        }
        self.inner.authenticated_identity()
    }
    fn page(&mut self, repo: &str, page: u32) -> Result<Vec<Value>, LookupError> {
        self.inner.page(repo, page)
    }
    fn repository_identity(&mut self, repo: &str) -> Result<u64, LookupError> {
        self.inner.repository_identity(repo)
    }
    fn detail(&mut self, repo: &str, number: u64) -> Result<Value, LookupError> {
        self.inner.detail(repo, number)
    }
}

struct Case {
    lane: Lane,
    launcher: Launcher,
    revision: &'static str,
    identity_drift: bool,
    local_mutation: Option<(&'static str, usize)>,
    audit_seen: Vec<bool>,
    actor: &'static str,
    identity_reads: usize,
}
impl Case {
    fn new() -> Self {
        Self {
            lane: amendment::fixture(),
            launcher: Launcher::default(),
            revision: "corrected-revision",
            identity_drift: false,
            local_mutation: None,
            audit_seen: Vec::new(),
            actor: "bot",
            identity_reads: 0,
        }
    }
    fn run_with(&mut self, launcher: &mut impl AmendedSupervisorLauncher) -> ContinuationResult {
        let lane = &mut self.lane;
        let mut saved_prs = std::mem::take(&mut lane.github.prs);
        let mut prs = Prs {
            inner: reads::Prs(&mut saved_prs, lane.read_scenario),
            calls: 0,
            identity_drift: self.identity_drift,
        };
        let result = amend_never_dispatched_initial_branch(
            &mut lane.f.store,
            AmendedContinuationDependencies {
                task_id: "task-a",
                attempt_id: "attempt-task-a",
                actor: self.actor,
                config: &lane.f.config,
                config_revision: self.revision,
                projects: &mut reads::Projects(&mut lane.github, lane.read_scenario),
                prs: &mut prs,
                local: &mut local::Checks {
                    inner: &mut lane.local,
                    mutation: self.local_mutation,
                    audit_seen: &mut self.audit_seen,
                },
                processes: &mut lane.processes,
                launcher,
            },
        )
        .unwrap();
        self.identity_reads = prs.calls;
        lane.github.prs = saved_prs;
        result
    }
    fn held(&mut self, reason: Refusal, audits: usize) {
        let mut launcher = std::mem::take(&mut self.launcher);
        assert_eq!(
            self.run_with(&mut launcher),
            ContinuationResult::Held(reason)
        );
        assert!(launcher.plans.is_empty());
        self.launcher = launcher;
        self.assert_held(audits, 0);
    }
    fn assert_held(&self, audits: usize, dispatches: usize) {
        let store = &self.lane.f.store;
        assert_eq!(scheduling::reservation_count(store).unwrap(), 1);
        assert_eq!(
            task_records::latest_attempt(store, "task-a")
                .unwrap()
                .as_deref(),
            Some("attempt-task-a")
        );
        assert_eq!(
            task_records::task_phase(store, "task-a")
                .unwrap()
                .as_deref(),
            Some("held")
        );
        assert_eq!(count(store, "evidence", "initial_branch_removed"), audits);
        assert_eq!(
            count(store, "intents", "initial_branch_removal_seal"),
            audits
        );
        assert_eq!(count(store, "intents", "supervisor_dispatch"), dispatches);
        for kind in [
            "attempt_exit",
            "exit_pr_lookup",
            "never_dispatched_authorized",
        ] {
            assert_eq!(count(store, "evidence", kind), 0);
        }
    }
}
fn count(store: &StateStore, table: &str, kind: &str) -> usize {
    let db = rusqlite::Connection::open(store.root().join("state.sqlite3")).unwrap();
    db.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE kind=?1"),
        [kind],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn amended_coordinator_audits_and_dispatches_exact_effective_plan_once() {
    let mut case = Case::new();
    let original = launches::launch_intent(&case.lane.f.store, "attempt-task-a")
        .unwrap()
        .unwrap();
    let config = case.lane.f.config.clone();
    let mut expected = case.lane.plan.clone();
    expected.args.drain(2..4);
    let mut launcher = Launcher::default();
    assert_eq!(
        case.run_with(&mut launcher),
        ContinuationResult::Dispatched(Box::new(expected.clone()))
    );
    assert_eq!(launcher.plans, [expected]);
    assert_eq!(case.lane.local.inspections, 2);
    assert_eq!(case.lane.processes.calls, 2);
    assert_eq!(case.lane.github.prs.lookups, 2);
    assert_eq!(case.identity_reads, 2);
    assert_eq!(case.audit_seen, [false, true]);
    assert_eq!(
        luthor::state::EffectiveConfigSnapshot::from(&case.lane.f.config),
        luthor::state::EffectiveConfigSnapshot::from(&config)
    );
    assert_eq!(
        launches::launch_intent(&case.lane.f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        original
    );
    case.assert_held(1, 1);
    assert_eq!(
        case.run_with(&mut launcher),
        ContinuationResult::Held(Refusal::Ineligible)
    );
    assert_eq!(launcher.plans.len(), 1);
    case.assert_held(1, 1);
}

#[test]
fn amended_coordinator_uncertain_launch_keeps_same_slot_and_never_relaunches() {
    let mut case = Case::new();
    let mut launcher = Launcher {
        fail: true,
        ..Launcher::default()
    };
    assert_eq!(
        case.run_with(&mut launcher),
        ContinuationResult::Held(Refusal::LaunchFailed)
    );
    case.assert_held(1, 1);
    assert_eq!(
        case.run_with(&mut launcher),
        ContinuationResult::Held(Refusal::Ineligible)
    );
    assert_eq!(launcher.plans.len(), 1);
}
