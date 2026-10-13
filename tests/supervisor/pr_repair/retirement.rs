//! Issue 54: tasks whose issue closed or PR finished are retired, not held,
//! and stop occupying capacity. A task that may be running never is.
use super::*;
use luthor::{
    coordinator::{abandon_task, retire_finished_tasks},
    eligibility::Candidate,
    github::project::{ProjectReadError, ReadCategory},
    pr_repair::UNSPECIFIC_REPAIR_HOLD_REASON,
    state::StateError,
};
use serde_json::{Value, json};

const CLAIM_CHANGED: &str = "PR repair held: issue claim changed or could not be re-read";
const POST_REPAIR: &str = "post-repair PR or claim verification failed; requires a person";

/// The tracker as GitHub reports it for the task's issue.
struct Tracker {
    candidate: Candidate,
    state: &'static str,
    unreadable: bool,
}
impl Tracker {
    fn of(f: &Fixture, state: &'static str) -> Self {
        let candidate = task_records::selection_evidence(&f.store, "task")
            .unwrap()
            .unwrap()
            .candidate;
        Self {
            candidate,
            state,
            unreadable: false,
        }
    }
}
impl ProjectReader for Tracker {
    fn page(&mut self, _: &str, _: Option<&str>) -> Result<Page<ProjectItem>, ProjectReadError> {
        OtherProject(self.candidate.clone(), 2).page("", None)
    }
    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError> {
        if self.unreadable {
            return Err(ProjectReadError {
                category: ReadCategory::Transport,
                ..String::from("offline").into()
            });
        }
        let mut issue = OtherProject(self.candidate.clone(), 2).issue(item)?;
        issue.state = self.state.into();
        issue.closed_at = (self.state == "closed").then(|| "2026-02-01T00:00:03Z".to_owned());
        Ok(issue)
    }
}

/// PR 42 read by its id. A retirement decision never consults the open-PR
/// search, so a sweep fails the test if it does; reconciliation looks the PR
/// up first and sees nothing listed.
struct ByIdPr {
    detail: Value,
    search_allowed: bool,
}
impl ByIdPr {
    fn state(f: &mut Fixture, state: &str) -> Self {
        let mut detail = f.prs.detail("org/code", 42).unwrap();
        detail["merged"] = json!(false);
        if state != "open" {
            detail["state"] = json!("closed");
            detail["closed_at"] = json!("2026-02-01T00:00:00Z");
            detail["merged"] = json!(state == "merged");
            detail["merged_at"] = if state == "merged" {
                json!("2026-02-01T00:00:00Z")
            } else {
                Value::Null
            };
            detail["merge_commit_sha"] = json!("0123abc");
        }
        Self {
            detail,
            search_allowed: false,
        }
    }
}
impl PullRequestReader for ByIdPr {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok("operator".into())
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<Value>, LookupError> {
        assert!(self.search_allowed, "the open-PR search was consulted");
        Ok(vec![])
    }
    fn detail(&mut self, _: &str, _: u64) -> Result<Value, LookupError> {
        Ok(self.detail.clone())
    }
    fn repository_identity(&mut self, _: &str) -> Result<u64, LookupError> {
        unreachable!("retirement compares saved identities only")
    }
}

fn phase(f: &Fixture) -> String {
    task_records::task_phase(&f.store, "task").unwrap().unwrap()
}

fn record(f: &Fixture) -> Value {
    let connection = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
    let payload: String = connection
        .query_row(
            "SELECT payload FROM evidence WHERE task_id='task' AND kind='task_retired'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&payload).unwrap()
}

fn cli(f: &Fixture, args: &[&str]) -> Value {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    serde_json::from_str(&luthor::cli::execute(&f.config.state_root, &args).unwrap()).unwrap()
}

fn slot_is_taken(f: &Fixture) -> bool {
    matches!(
        scheduling::ensure_dispatch_capacity(&f.store),
        Err(StateError::Capacity { .. })
    )
}

fn sweep(
    f: &mut Fixture,
    issue_state: &'static str,
    prs: &mut ByIdPr,
) -> luthor::coordinator::RetirementReport {
    let mut tracker = Tracker::of(f, issue_state);
    retire_finished_tasks(&mut f.store, &mut tracker, prs).unwrap()
}

#[test]
fn a_closed_issue_retires_tasks_in_every_non_running_phase_and_frees_the_slot() {
    for (stuck_phase, reason) in [
        ("attention", None),
        ("paused", None),
        ("held", Some(CLAIM_CHANGED)),
        ("held", Some(POST_REPAIR)),
        ("held", Some(UNSPECIFIC_REPAIR_HOLD_REASON)),
    ] {
        let mut f = Fixture::new();
        match reason {
            Some(reason) => task_records::hold_task(&mut f.store, "task", reason).unwrap(),
            None => task_records::set_task_phase(&mut f.store, "task", stuck_phase).unwrap(),
        }
        assert!(slot_is_taken(&f), "{stuck_phase} {reason:?}");
        let before = journal::evidence_kinds(&f.store, "task").unwrap();
        let mut prs = ByIdPr::state(&mut f, "open");
        let report = sweep(&mut f, "closed", &mut prs);
        assert_eq!(report.retired.len(), 1, "{stuck_phase} {reason:?}");
        assert!(report.retained.is_empty());
        assert_eq!(phase(&f), "issue_closed");
        assert!(!slot_is_taken(&f), "{stuck_phase} {reason:?}");
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
        let after = journal::evidence_kinds(&f.store, "task").unwrap();
        assert_eq!(after[..before.len()], before[..], "no row is deleted");
        assert_eq!(after[before.len()..], ["task_retired"]);
        let record = record(&f);
        assert_eq!(record["previous_phase"], stuck_phase);
        assert_eq!(record["evidence"]["issue"]["state"], "closed");
        assert_eq!(
            record["evidence"]["issue"]["closed_at"],
            "2026-02-01T00:00:03Z"
        );
        assert_eq!(record["evidence"]["pr"]["state"], "open");
        assert!(record["observed_at_unix_secs"].as_u64().unwrap() > 0);
    }
}

#[test]
fn a_merged_or_closed_saved_pr_retires_the_task_without_consulting_the_search() {
    for (pr_state, expected) in [("merged", "pr_merged"), ("closed", "pr_closed")] {
        let mut f = Fixture::new();
        task_records::hold_task(&mut f.store, "task", CLAIM_CHANGED).unwrap();
        let mut prs = ByIdPr::state(&mut f, pr_state);
        let report = sweep(&mut f, "open", &mut prs);
        assert_eq!(report.retired.len(), 1, "{pr_state}");
        assert_eq!(phase(&f), expected);
        assert!(!slot_is_taken(&f));
        let record = record(&f);
        assert_eq!(record["evidence"]["pr"]["state"], pr_state);
        assert_eq!(record["evidence"]["pr"]["number"], 42);
        if pr_state == "merged" {
            assert_eq!(record["evidence"]["pr"]["merge_commit_sha"], "0123abc");
        }
        assert_eq!(record["evidence"]["issue"]["state"], "open");
    }
}

#[test]
fn a_merged_pr_wins_over_a_closed_issue_and_keeps_both_observations() {
    let mut f = Fixture::new();
    task_records::hold_task(&mut f.store, "task", POST_REPAIR).unwrap();
    let mut prs = ByIdPr::state(&mut f, "merged");
    sweep(&mut f, "closed", &mut prs);
    assert_eq!(phase(&f), "pr_merged");
    let record = record(&f);
    assert_eq!(record["evidence"]["pr"]["state"], "merged");
    assert_eq!(
        record["evidence"]["issue"]["closed_at"],
        "2026-02-01T00:00:03Z"
    );
}

#[test]
fn genuinely_held_work_with_an_open_issue_and_open_pr_stays_held() {
    let mut f = Fixture::new();
    task_records::hold_task(
        &mut f.store,
        "task",
        "PR repair held: PR or check state could not be verified",
    )
    .unwrap();
    let before = journal::evidence_kinds(&f.store, "task").unwrap();
    let mut prs = ByIdPr::state(&mut f, "open");
    let report = sweep(&mut f, "open", &mut prs);
    assert!(report.retired.is_empty() && report.retained.is_empty());
    assert_eq!(phase(&f), "held");
    assert_eq!(journal::evidence_kinds(&f.store, "task").unwrap(), before);
    assert!(slot_is_taken(&f));
}

#[test]
fn unreadable_github_state_retains_the_task_with_the_reason() {
    let mut f = Fixture::new();
    task_records::hold_task(&mut f.store, "task", CLAIM_CHANGED).unwrap();
    let mut tracker = Tracker::of(&f, "closed");
    tracker.unreadable = true;
    let mut prs = ByIdPr::state(&mut f, "open");
    let report = retire_finished_tasks(&mut f.store, &mut tracker, &mut prs).unwrap();
    assert!(report.retired.is_empty());
    assert_eq!(report.retained.len(), 1);
    assert!(
        report.retained[0]
            .reason
            .starts_with("finished state not established")
    );
    assert_eq!(phase(&f), "held");
}

#[test]
fn a_task_with_an_attempt_that_has_no_verified_exit_is_never_retired() {
    let mut f = Fixture::new();
    scheduling::reserve(&mut f.store, "task", "attempt-live").unwrap();
    task_records::hold_task(&mut f.store, "task", CLAIM_CHANGED).unwrap();
    let mut prs = ByIdPr::state(&mut f, "merged");
    let report = sweep(&mut f, "closed", &mut prs);
    assert!(report.retired.is_empty() && report.retained.is_empty());
    assert_eq!(phase(&f), "held");
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    let refused = abandon_task(&mut f.store, "task", "operator", "wrong scope").unwrap_err();
    assert!(
        matches!(refused, StateError::NotRetirable(reason) if reason.contains("running task")),
        "{refused:?}"
    );
    assert_eq!(phase(&f), "held");
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
}

#[test]
fn abandon_records_actor_and_reason_frees_the_slot_and_is_shown() {
    let mut f = Fixture::new();
    task_records::set_task_phase(&mut f.store, "task", "attention").unwrap();
    assert!(slot_is_taken(&f));
    let abandoned = abandon_task(&mut f.store, "task", "alice", "duplicate of #12").unwrap();
    assert_eq!(abandoned.reason, "duplicate of #12");
    assert_eq!(phase(&f), "abandoned");
    assert!(!slot_is_taken(&f));
    let record = record(&f);
    assert_eq!(record["actor"], "alice");
    assert_eq!(record["reason"], "duplicate of #12");
    assert_eq!(record["evidence"]["github_changes"], "none");
    let status = cli(&f, &["status"]);
    assert_eq!(status["tasks"][0]["phase"], "abandoned");
    assert_eq!(status["tasks"][0]["reason"], "duplicate of #12");
    assert_eq!(status["tasks"][0]["terminal"]["actor"], "alice");
    assert_eq!(status["reserved_slot_count"], 0);
    let shown = cli(&f, &["show", "task"]);
    assert_eq!(shown["phase"], "abandoned");
    assert_eq!(shown["reason"], "duplicate of #12");
    assert_eq!(shown["terminal"]["phase"], "abandoned");
    assert!(
        shown["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "task_retired")
    );
}

#[test]
fn a_retired_task_cannot_be_retired_again_held_or_reconciled_back() {
    let mut f = Fixture::new();
    abandon_task(&mut f.store, "task", "alice", "wrong scope").unwrap();
    let again = abandon_task(&mut f.store, "task", "bob", "again").unwrap_err();
    assert!(matches!(
        again,
        StateError::NotRetirable("task is already retired")
    ));
    assert!(task_records::hold_task(&mut f.store, "task", "late hold").is_err());
    assert_eq!(phase(&f), "abandoned");
    let reconciled = f.reconcile("attempt-real");
    assert!(matches!(reconciled, Reconciliation::Held { reason } if reason.contains("retired")));
    assert_eq!(phase(&f), "abandoned");
    assert!(scheduling::pending_attempts(&f.store).unwrap().is_empty());
    assert!(scheduling::unresolved_sources(&f.store).unwrap().is_empty());
}

#[test]
fn search_lagging_an_open_pr_while_the_issue_is_closed_retires_instead_of_holding() {
    let mut f = Fixture::new();
    let mut closed = Tracker::of(&f, "closed");
    let still_open = ByIdPr::state(&mut f, "open").detail;
    let mut lagging = LaggingSearch {
        open: &mut f.prs,
        later: still_open,
        detail_reads: 0,
    };
    let outcome = repair_verified_prs(
        &mut f.store,
        &f.config,
        &mut closed,
        &mut lagging,
        &mut OtherLauncher::default(),
        &mut f.ids,
    )
    .unwrap();
    assert!(outcome.held.is_empty() && outcome.launched.is_empty());
    assert_eq!(outcome.retired.len(), 1);
    assert_eq!(phase(&f), "issue_closed");
    assert!(!slot_is_taken(&f));
    assert_eq!(record(&f)["evidence"]["pr"]["state"], "open");
}

#[test]
fn a_repair_failure_on_a_pr_already_merged_is_left_to_cleanup_not_held() {
    let mut f = Fixture::new();
    let mut closed = Tracker::of(&f, "closed");
    let merged = ByIdPr::state(&mut f, "merged").detail;
    let mut lagging = LaggingSearch {
        open: &mut f.prs,
        later: merged,
        detail_reads: 0,
    };
    let outcome = repair_verified_prs(
        &mut f.store,
        &f.config,
        &mut closed,
        &mut lagging,
        &mut OtherLauncher::default(),
        &mut f.ids,
    )
    .unwrap();
    assert!(outcome.held.is_empty() && outcome.retired.is_empty());
    assert_eq!(outcome.skipped.len(), 1);
    assert!(outcome.skipped[0].reason.contains("merged"));
    assert_eq!(phase(&f), "pr_complete");
}

fn repaired_attempt(f: &mut Fixture) -> luthor::supervisor::LaunchPlan {
    let plan = f.repair().unwrap().remove(0);
    git(&plan.worktree, &["commit", "--allow-empty", "-m", "repair"]);
    f.prs.head = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&plan.worktree)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .into();
    f.run(&plan, "repaired");
    plan
}

fn reconcile_with<P: ProjectReader, Q: PullRequestReader>(
    f: &mut Fixture,
    attempt: &str,
    projects: &mut P,
    prs: &mut Q,
) -> Reconciliation {
    luthor::coordinator::reconcile_with_pr(&mut f.store, "task", attempt, projects, prs).unwrap()
}

#[test]
fn a_pr_merged_during_a_repair_ends_the_task_instead_of_holding_it() {
    let mut f = Fixture::new();
    let plan = repaired_attempt(&mut f);
    let mut prs = ByIdPr::state(&mut f, "merged");
    prs.search_allowed = true;
    let mut tracker = Tracker::of(&f, "open");
    let result = reconcile_with(&mut f, &plan.attempt_id, &mut tracker, &mut prs);
    assert!(
        matches!(result, Reconciliation::Completed { .. }),
        "{result:?}"
    );
    assert_eq!(phase(&f), "pr_merged");
    assert!(!slot_is_taken(&f));
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert!(
        !journal::evidence_kinds(&f.store, "task")
            .unwrap()
            .contains(&"pr_repair_blocked".into())
    );
    assert!(scheduling::pending_attempts(&f.store).unwrap().is_empty());
    assert_eq!(record(&f)["evidence"]["pr"]["state"], "merged");
}

#[test]
fn an_issue_closed_during_a_repair_ends_the_task_even_while_search_lists_the_pr() {
    let mut f = Fixture::new();
    let plan = repaired_attempt(&mut f);
    let mut closed = Tracker::of(&f, "closed");
    let result = luthor::coordinator::reconcile_with_pr(
        &mut f.store,
        "task",
        &plan.attempt_id,
        &mut closed,
        &mut f.prs,
    )
    .unwrap();
    assert!(
        matches!(result, Reconciliation::Completed { .. }),
        "{result:?}"
    );
    assert_eq!(phase(&f), "issue_closed");
    assert!(!slot_is_taken(&f));
    assert!(
        !journal::evidence_kinds(&f.store, "task")
            .unwrap()
            .contains(&"pr_repair_blocked".into())
    );
}

#[test]
fn a_repair_held_for_a_person_is_retired_once_its_pr_is_found_merged() {
    let mut f = Fixture::new();
    let plan = repaired_attempt(&mut f);
    let mut open = ByIdPr::state(&mut f, "open");
    open.search_allowed = true;
    let mut tracker = Tracker::of(&f, "open");
    let held = reconcile_with(&mut f, &plan.attempt_id, &mut tracker, &mut open);
    assert!(matches!(held, Reconciliation::Held { .. }), "{held:?}");
    assert_eq!(phase(&f), "held");
    assert_eq!(
        task_records::held_reason(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some(POST_REPAIR)
    );
    assert!(slot_is_taken(&f));
    let mut merged = ByIdPr::state(&mut f, "merged");
    merged.search_allowed = true;
    let done = reconcile_with(&mut f, &plan.attempt_id, &mut tracker, &mut merged);
    assert!(matches!(done, Reconciliation::Completed { .. }), "{done:?}");
    assert_eq!(phase(&f), "pr_merged");
    assert!(!slot_is_taken(&f));
    assert_eq!(record(&f)["previous_hold_reason"], POST_REPAIR);
    assert!(
        journal::evidence_kinds(&f.store, "task")
            .unwrap()
            .contains(&"pr_repair_blocked".into())
    );
}

/// The search lists PR 42 as open on the lookup; every later read by id
/// returns `later`, as when the PR merged in between.
struct LaggingSearch<'a> {
    open: &'a mut RepairPr,
    later: Value,
    detail_reads: usize,
}
impl PullRequestReader for LaggingSearch<'_> {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        self.open.authenticated_identity()
    }
    fn page(&mut self, repo: &str, page: u32) -> Result<Vec<Value>, LookupError> {
        self.open.page(repo, page)
    }
    fn detail(&mut self, repo: &str, number: u64) -> Result<Value, LookupError> {
        self.detail_reads += 1;
        // The first read is the lookup that still sees the PR open; every
        // later read is by id after the merge.
        if self.detail_reads == 1 {
            return self.open.detail(repo, number);
        }
        Ok(self.later.clone())
    }
    fn repository_identity(&mut self, repo: &str) -> Result<u64, LookupError> {
        self.open.repository_identity(repo)
    }
}
impl PrHealthReader for LaggingSearch<'_> {
    fn rerun_failed_jobs(&mut self, a: &str, b: u64, c: &str) -> Result<(), LookupError> {
        self.open.rerun_failed_jobs(a, b, c)
    }
    fn failed_job_logs(&mut self, a: &str, b: u64) -> Result<String, LookupError> {
        self.open.failed_job_logs(a, b)
    }
    fn health(
        &mut self,
        a: &str,
        b: u64,
        checks: &luthor::config::RequiredChecks,
    ) -> Result<PrHealth, LookupError> {
        self.open.health(a, b, checks)
    }
}
