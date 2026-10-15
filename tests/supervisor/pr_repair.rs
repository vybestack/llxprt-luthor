#![cfg(unix)]
//! Existing PR repair through the real supervisor and durable receipt path.
use super::*;
use luthor::{
    coordinator::repair_verified_prs,
    github::pull_request::{FailedJobsRerun, PrHealth, PrHealthReader},
    pr_repair::{MAX_PR_REPAIRS, REPAIR_RESULT_FILE},
    state::{journal, scheduling, task_records, worktree_records},
};
use std::process::Command;

struct RepairPr {
    inner: ExitPr,
    head: String,
    conflict: bool,
    failures: Vec<String>,
    health_error: bool,
    wrong_health_head: bool,
    wrong_pr_id: bool,
    run_attempt: u64,
    reruns: usize,
    pending: bool,
    rerun_error: bool,
    logs_error: bool,
    gh_health: Option<luthor::github::pull_request::GhPullRequestReader>,
}
impl PullRequestReader for RepairPr {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        self.inner.authenticated_identity()
    }
    fn page(&mut self, repo: &str, page: u32) -> Result<Vec<serde_json::Value>, LookupError> {
        self.inner.page(repo, page)
    }
    fn detail(&mut self, repo: &str, number: u64) -> Result<serde_json::Value, LookupError> {
        let mut detail = self.inner.detail(repo, number)?;
        detail["head"]["sha"] = serde_json::json!(self.head);
        if self.wrong_pr_id {
            detail["id"] = serde_json::json!(9999);
        }
        Ok(detail)
    }
    fn repository_identity(&mut self, repo: &str) -> Result<u64, LookupError> {
        self.inner.repository_identity(repo)
    }
}
impl PrHealthReader for RepairPr {
    fn rerun_failed_jobs<E: From<LookupError>>(
        &mut self,
        repo: &str,
        run: u64,
        head: &str,
        before_post: impl FnOnce() -> Result<(), E>,
    ) -> Result<FailedJobsRerun, E> {
        self.reruns += 1;
        if let Some(gh) = &mut self.gh_health {
            return gh.rerun_failed_jobs(repo, run, head, before_post);
        }
        before_post()?;
        if self.rerun_error {
            return Err(LookupError {
                category: ErrorCategory::Transport,
                code: "offline",
                status: None,
            }
            .into());
        }
        Ok(FailedJobsRerun::Accepted)
    }
    fn failed_job_logs(&mut self, repo: &str, job: u64) -> Result<String, LookupError> {
        if let Some(gh) = &mut self.gh_health {
            return gh.failed_job_logs(repo, job);
        }
        if self.logs_error {
            return Err(LookupError {
                category: ErrorCategory::Transport,
                code: "offline",
                status: None,
            });
        }
        Ok("assertion failed: expected behavior preserved".into())
    }
    fn health(
        &mut self,
        repo: &str,
        number: u64,
        checks: &luthor::config::RequiredChecks,
    ) -> Result<PrHealth, LookupError> {
        if let Some(gh) = &mut self.gh_health {
            return gh.health(repo, number, checks);
        }
        if self.health_error {
            return Err(LookupError {
                category: ErrorCategory::Transport,
                code: "offline",
                status: None,
            });
        }
        Ok(PrHealth {
            id: 4242,
            head: if self.wrong_health_head {
                "wrong".into()
            } else {
                self.head.clone()
            },
            conflicting: self.conflict,
            failed_required_checks: self.failures.clone(),
            required_checks_pending: self.pending,
            required_checks_source: luthor::github::pull_request::RequiredChecksSource::Github,
            required_jobs: vec![luthor::github::pull_request::FailedJob {
                name: "quality gates".into(),
                run_id: 123,
                run_attempt: self.run_attempt,
                job_id: 456,
            }],
            failed_jobs: self
                .failures
                .iter()
                .map(|name| luthor::github::pull_request::FailedJob {
                    name: name.clone(),
                    run_id: 123,
                    run_attempt: self.run_attempt,
                    job_id: 456,
                })
                .collect(),
        })
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    config: Config,
    store: StateStore,
    projects: OtherProject,
    prs: RepairPr,
    ids: OtherIds,
}
impl Fixture {
    fn new() -> Self {
        Self::with_checks(Default::default())
    }
    fn with_checks(checks: luthor::config::RequiredChecks) -> Self {
        let (dir, config, mut store) = execution::dispatched_fixture_with_checks(0, checks);
        let selection = task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap();
        let identity = worktree_records::worktree_record(&store, "task")
            .unwrap()
            .unwrap()
            .identity
            .unwrap();
        let mut prs = RepairPr {
            inner: ExitPr {
                matching: Some((selection.candidate.issue_url.clone(), identity.branch)),
                ..Default::default()
            },
            head: identity.head,
            conflict: true,
            failures: vec![],
            health_error: false,
            wrong_health_head: false,
            wrong_pr_id: false,
            run_attempt: 1,
            reruns: 0,
            pending: false,
            rerun_error: false,
            logs_error: false,
            gh_health: None,
        };
        let mut projects = OtherProject(selection.candidate, 1);
        assert!(matches!(
            luthor::coordinator::reconcile_with_pr(
                &mut store,
                "task",
                "attempt-real",
                &mut projects,
                &mut prs
            )
            .unwrap(),
            Reconciliation::Completed { .. }
        ));
        assert_eq!(
            task_records::task_phase(&store, "task").unwrap().as_deref(),
            Some("pr_complete")
        );
        Self {
            _dir: dir,
            config,
            store,
            projects,
            prs,
            ids: OtherIds::default(),
        }
    }
    fn repair_outcome(&mut self) -> luthor::coordinator::RepairOutcome {
        repair_verified_prs(
            &mut self.store,
            &self.config,
            &mut self.projects,
            &mut self.prs,
            &mut OtherLauncher::default(),
            &mut self.ids,
        )
        .unwrap()
    }
    /// The launched plans, or the notes of the tasks the pass held.
    fn repair(
        &mut self,
    ) -> Result<Vec<luthor::supervisor::LaunchPlan>, Vec<luthor::coordinator::TaskNote>> {
        let outcome = self.repair_outcome();
        if outcome.held.is_empty() {
            Ok(outcome.launched)
        } else {
            Err(outcome.held)
        }
    }
    fn run(&mut self, plan: &luthor::supervisor::LaunchPlan, status: &str) {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' '{{\"attempt_id\":\"{}\",\"status\":\"{status}\"}}' > {REPAIR_RESULT_FILE}\n",
            plan.attempt_id
        );
        fs::write(plan.executable.with_extension("behavior"), script).unwrap();
        let owner = luthor::WorktreeOwner::acquire_existing(self.store.root(), "task").unwrap();
        execute_with_binary(
            &mut self.store,
            plan,
            Path::new(env!("CARGO_BIN_EXE_luthor")),
            &owner,
        )
        .unwrap();
        drop(owner);
        wait_for_receipt(&self.config, &self.store, &plan.attempt_id);
        supervisor_support::wait_for_worktree_owner_release(self.store.root(), "task");
    }
    fn reconcile(&mut self, attempt: &str) -> Reconciliation {
        luthor::coordinator::reconcile_with_pr(
            &mut self.store,
            "task",
            attempt,
            &mut self.projects,
            &mut self.prs,
        )
        .unwrap()
    }
}

fn wait_for_receipt(config: &Config, store: &StateStore, attempt: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let path = config
        .state_root
        .join(format!("attempts/{attempt}.receipt.json"));
    while !path.exists() {
        assert!(Instant::now() < deadline, "repair receipt missing");
        thread::sleep(Duration::from_millis(10));
    }
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let ready = journal::evidence_payloads(store, "task", attempt, "supervisor_ready").unwrap();
    assert_eq!(ready.len(), 1);
    let ready = &ready[0];
    let ready: serde_json::Value = serde_json::from_str(ready).unwrap();
    wait_for_process_and_group_absence(
        libc::pid_t::try_from(ready["pid"].as_u64().unwrap()).unwrap(),
    );
    wait_for_process_and_group_absence(libc::pid_t::try_from(receipt.child_pid).unwrap());
}

#[test]
fn conflicting_pr_continues_same_session_and_reverifies_pushed_head() {
    let mut f = Fixture::new();
    let plan = f.repair().unwrap().remove(0);
    assert_eq!(plan.session_id, "task");
    assert_eq!(plan.task_id, "task");
    let original: luthor::supervisor::LaunchPlan = serde_json::from_str(
        &luthor::state::launches::launch_intent(&f.store, "attempt-real")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(plan.worktree, original.worktree);
    assert_eq!(plan.session_environment, original.session_environment);
    let prompt = plan.args.last().unwrap();
    for text in [
        "merge only, no rebase, no force push",
        "BOTH sides",
        "ALL repository gates",
        "semantic_conflict",
        "not a new PR",
    ] {
        assert!(prompt.contains(text), "missing {text}");
    }
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert!(
        journal::evidence_kinds(&f.store, "task")
            .unwrap()
            .contains(&"pr_repair_authorized".into())
    );
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
    assert!(matches!(
        f.reconcile(&plan.attempt_id),
        Reconciliation::Completed { .. }
    ));
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    assert!(scheduling::ensure_dispatch_capacity(&f.store).is_ok());
    assert!(scheduling::pending_attempts(&f.store).unwrap().is_empty());
    assert!(matches!(
        f.reconcile(&plan.attempt_id),
        Reconciliation::Completed { .. }
    ));
    assert_eq!(
        journal::evidence_payloads(&f.store, "task", &plan.attempt_id, "verified_open_pr")
            .unwrap()
            .len(),
        1
    );
    let shown: serde_json::Value = serde_json::from_str(
        &luthor::cli::execute(&f.config.state_root, &["show".into(), "task".into()]).unwrap(),
    )
    .unwrap();
    assert_eq!(shown["verified_pr"]["attempt_id"], plan.attempt_id);
    f.prs.conflict = false;
    assert!(f.repair().unwrap().is_empty());
}

#[test]
fn required_check_failure_repairs_and_limit_survives_restart() {
    let mut f = Fixture::new();
    f.prs.conflict = false;
    f.prs.failures = vec!["quality gates".into()];
    assert!(f.repair().unwrap().is_empty());
    assert_eq!(f.prs.reruns, 1);
    assert!(f.repair().unwrap().is_empty());
    assert_eq!(f.prs.reruns, 1);
    f.prs.run_attempt = 2;
    for _ in 0..MAX_PR_REPAIRS {
        let plan = f.repair().unwrap().remove(0);
        assert!(plan.args.last().unwrap().contains("quality gates"));
        assert!(plan.args.last().unwrap().contains("assertion failed"));
        f.run(&plan, "repaired");
        assert!(matches!(
            f.reconcile(&plan.attempt_id),
            Reconciliation::Completed { .. }
        ));
    }
    drop(f.store);
    f.store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert!(f.repair().unwrap().is_empty());
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert!(
        task_records::held_reason(&f.store, "task")
            .unwrap()
            .unwrap()
            .contains("attempt limit")
    );
    assert!(
        task_records::held_reason(&f.store, "task")
            .unwrap()
            .unwrap()
            .contains("quality gates")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}

#[test]
fn semantic_conflict_holds_instead_of_completing_or_retrying() {
    let mut f = Fixture::new();
    let plan = f.repair().unwrap().remove(0);
    f.run(&plan, "semantic_conflict");
    assert!(matches!(
        f.reconcile(&plan.attempt_id),
        Reconciliation::Held { .. }
    ));
    assert!(f.repair().unwrap().is_empty());
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}

#[test]
fn unhealthy_pr_with_uncertain_identity_or_claim_never_reserves() {
    for scenario in [
        "health_error",
        "health_head",
        "author",
        "ambiguous",
        "claim",
        "local_head",
    ] {
        let mut f = Fixture::new();
        match scenario {
            "health_error" => f.prs.health_error = true,
            "health_head" => f.prs.wrong_health_head = true,
            "author" => f.prs.inner.author = Some("other".into()),
            "ambiguous" => f.prs.inner.ambiguous = true,
            "claim" => f.projects.1 = 0,
            "local_head" => {
                let worktree = worktree_records::worktree_record(&f.store, "task")
                    .unwrap()
                    .unwrap()
                    .identity
                    .unwrap();
                git(
                    &worktree.path,
                    &["commit", "--allow-empty", "-m", "external"],
                );
            }
            _ => unreachable!(),
        }
        assert!(f.repair().is_err(), "{scenario}");
        assert_eq!(
            scheduling::reservation_count(&f.store).unwrap(),
            0,
            "{scenario}"
        );
        assert_eq!(
            task_records::task_phase(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("held"),
            "{scenario}"
        );
    }
}

#[test]
fn required_checks_passing_on_rerun_are_flaky_looking_without_worker() {
    let mut f = Fixture::new();
    f.prs.conflict = false;
    f.prs.failures = vec!["quality gates".into()];
    assert!(f.repair().unwrap().is_empty());
    assert_eq!(f.prs.reruns, 1);
    f.prs.failures.clear();
    f.prs.run_attempt = 2;
    f.prs.pending = true;
    assert!(f.repair().unwrap().is_empty());
    assert!(
        !journal::evidence_kinds(&f.store, "task")
            .unwrap()
            .contains(&"pr_checks_flaky_looking".into())
    );
    f.prs.pending = false;
    assert!(f.repair().unwrap().is_empty());
    assert!(
        journal::evidence_kinds(&f.store, "task")
            .unwrap()
            .contains(&"pr_checks_flaky_looking".into())
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}

#[test]
fn ambiguous_job_rerun_or_unreadable_logs_hold_without_worker() {
    for rerun_error in [true, false] {
        let mut f = Fixture::new();
        f.prs.conflict = false;
        f.prs.failures = vec!["quality gates".into()];
        f.prs.rerun_error = rerun_error;
        if !rerun_error {
            assert!(f.repair().unwrap().is_empty());
            f.prs.run_attempt = 2;
            f.prs.logs_error = true;
        }
        assert!(f.repair().is_err());
        assert!(f.repair().unwrap().is_empty());
        assert_eq!(f.prs.reruns, 1);
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            task_records::task_phase(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("held")
        );
    }
}

#[test]
fn semantic_conflict_hold_survives_reconcile_and_result_changes() {
    let mut f = Fixture::new();
    let plan = f.repair().unwrap().remove(0);
    f.run(&plan, "semantic_conflict");
    assert!(matches!(
        f.reconcile(&plan.attempt_id),
        Reconciliation::Held { .. }
    ));
    drop(f.store);
    f.store = StateStore::open(&f.config.state_root, 1).unwrap();
    fs::write(
        plan.worktree.join(REPAIR_RESULT_FILE),
        format!(
            "{{\"attempt_id\":\"{}\",\"status\":\"repaired\"}}",
            plan.attempt_id
        ),
    )
    .unwrap();
    assert!(matches!(
        f.reconcile(&plan.attempt_id),
        Reconciliation::Held { .. }
    ));
    assert!(f.repair().unwrap().is_empty());
}

#[test]
fn active_repair_worker_is_not_stopped_by_its_existing_pr() {
    let mut f = Fixture::new();
    let plan = f.repair().unwrap().remove(0);
    let release = plan.worktree.join("release-worker");
    fs::write(plan.executable.with_extension("behavior"), format!("#!/bin/sh\nwhile ! test -f '{}'; do sleep 0.05; done\nprintf '%s\\n' '{{\"attempt_id\":\"{}\",\"status\":\"repaired\"}}' > {REPAIR_RESULT_FILE}\n", release.display(), plan.attempt_id)).unwrap();
    let owner = luthor::WorktreeOwner::acquire_existing(f.store.root(), "task").unwrap();
    execute_with_binary(
        &mut f.store,
        &plan,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    let result = f.reconcile(&plan.attempt_id);
    fs::write(release, "release").unwrap();
    wait_for_receipt(&f.config, &f.store, &plan.attempt_id);
    supervisor_support::wait_for_worktree_owner_release(f.store.root(), "task");
    assert_eq!(result, Reconciliation::Running);
    assert!(
        journal::stop_intent(&f.store, "task", &plan.attempt_id)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        f.reconcile(&plan.attempt_id),
        Reconciliation::Completed { .. }
    ));
}

#[test]
fn missing_or_stale_repair_result_and_unpushed_tip_fail_closed() {
    for scenario in ["missing", "stale", "unpushed", "symlink"] {
        let mut f = Fixture::new();
        let plan = f.repair().unwrap().remove(0);
        f.run(&plan, "repaired");
        let path = plan.worktree.join(REPAIR_RESULT_FILE);
        match scenario {
            "missing" => fs::remove_file(path).unwrap(),
            "stale" => fs::write(
                path,
                "{\"attempt_id\":\"attempt-real\",\"status\":\"repaired\"}",
            )
            .unwrap(),
            "symlink" => {
                let elsewhere = plan.worktree.join("result-target");
                fs::rename(&path, &elsewhere).unwrap();
                std::os::unix::fs::symlink(elsewhere, path).unwrap();
            }
            "unpushed" => git(
                &plan.worktree,
                &["commit", "--allow-empty", "-m", "not-pushed"],
            ),
            _ => unreachable!(),
        }
        assert!(
            luthor::coordinator::reconcile_with_pr(
                &mut f.store,
                "task",
                &plan.attempt_id,
                &mut f.projects,
                &mut f.prs
            )
            .is_err()
        );
        assert_eq!(
            task_records::task_phase(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("held")
        );
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
        assert!(f.repair().unwrap().is_empty());
    }
}

#[test]
fn changed_repair_config_and_missing_previous_receipt_do_not_launch() {
    for config_change in [true, false] {
        let mut f = Fixture::new();
        if config_change {
            // Issue 51: drift on an open PR is skipped with its reason, not held.
            f.config.assignment_login = "other".into();
            let outcome = f.repair_outcome();
            assert!(outcome.launched.is_empty() && outcome.held.is_empty());
            assert_eq!(outcome.skipped.len(), 1);
            assert!(outcome.skipped[0].reason.contains("assignment_login"));
            assert_eq!(
                task_records::task_phase(&f.store, "task")
                    .unwrap()
                    .as_deref(),
                Some("pr_complete")
            );
        } else {
            fs::remove_file(receipt_path(&f.config)).unwrap();
            assert!(f.repair().is_err());
        }
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    }
}

#[test]
fn missing_or_replaced_pr_after_repair_stays_held_without_normal_retry() {
    for scenario in ["absent", "ambiguous", "changed_id", "claim"] {
        let mut f = Fixture::new();
        let plan = f.repair().unwrap().remove(0);
        f.run(&plan, "repaired");
        match scenario {
            "absent" => f.prs.inner.matching = None,
            "ambiguous" => f.prs.inner.ambiguous = true,
            "changed_id" => f.prs.wrong_pr_id = true,
            "claim" => f.projects.1 = 0,
            _ => unreachable!(),
        }
        let result = luthor::coordinator::reconcile_with_pr(
            &mut f.store,
            "task",
            &plan.attempt_id,
            &mut f.projects,
            &mut f.prs,
        );
        assert!(
            !matches!(result, Ok(Reconciliation::Completed { .. })),
            "{scenario}"
        );
        assert_eq!(
            task_records::task_phase(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("held")
        );
        assert!(matches!(
            f.reconcile(&plan.attempt_id),
            Reconciliation::Held { .. }
        ));
        assert!(f.repair().unwrap().is_empty());
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
        assert!(
            journal::evidence_kinds(&f.store, "task")
                .unwrap()
                .contains(&"pr_repair_blocked".into())
        );
    }
}

#[test]
fn active_repair_with_disappearing_pr_is_held_and_capacity_retained() {
    let mut f = Fixture::new();
    let plan = f.repair().unwrap().remove(0);
    let release = plan.worktree.join("release-worker");
    fs::write(plan.executable.with_extension("behavior"), format!("#!/bin/sh\nwhile ! test -f '{}'; do sleep 0.05; done\nprintf '%s\\n' '{{\"attempt_id\":\"{}\",\"status\":\"repaired\"}}' > {REPAIR_RESULT_FILE}\n", release.display(), plan.attempt_id)).unwrap();
    let owner = luthor::WorktreeOwner::acquire_existing(f.store.root(), "task").unwrap();
    execute_with_binary(
        &mut f.store,
        &plan,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    f.prs.inner.matching = None;
    let result = f.reconcile(&plan.attempt_id);
    let count = scheduling::reservation_count(&f.store).unwrap();
    fs::write(release, "release").unwrap();
    wait_for_receipt(&f.config, &f.store, &plan.attempt_id);
    supervisor_support::wait_for_worktree_owner_release(f.store.root(), "task");
    assert!(matches!(result, Reconciliation::Held { .. }));
    assert_eq!(count, 1);
    assert!(
        journal::stop_intent(&f.store, "task", &plan.attempt_id)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        f.reconcile(&plan.attempt_id),
        Reconciliation::Held { .. }
    ));
}

#[test]
fn pending_required_check_waits_without_coding_or_rerun() {
    let mut f = Fixture::new();
    f.prs.conflict = false;
    f.prs.pending = true;
    assert!(f.repair().unwrap().is_empty());
    assert_eq!(f.prs.reruns, 0);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
}

#[test]
fn base_conflict_repairs_even_when_checks_are_still_pending() {
    let mut f = Fixture::new();
    f.prs.pending = true;
    let plan = f.repair().unwrap().remove(0);
    assert!(plan.args.last().unwrap().contains("conflicting=true"));
    assert_eq!(f.prs.reruns, 0);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
}

#[test]
fn unacknowledged_job_rerun_intent_holds_instead_of_repeating_post() {
    let mut f = Fixture::new();
    f.prs.conflict = false;
    f.prs.failures = vec!["quality gates".into()];
    let payload = serde_json::json!({"head":f.prs.head,"run_id":123,"run_attempt":1,"checks":["quality gates"]});
    journal::record_evidence(
        &mut f.store,
        "task",
        Some("attempt-real"),
        "pr_failed_jobs_rerun",
        &payload.to_string(),
    )
    .unwrap();
    assert!(f.repair().is_err());
    assert_eq!(f.prs.reruns, 0);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("held")
    );
}

mod configured_checks;
mod history;
mod pending_runs;
mod retirement;

#[test]
fn worker_pin_repair_keeps_build_after_operator_updates_templates() {
    let mut f = Fixture::new();
    let original: luthor::supervisor::LaunchPlan = serde_json::from_str(
        &luthor::state::launches::launch_intent(&f.store, "attempt-real")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    f.config.initial.executable = f
        .config
        .worktree_root
        .parent()
        .unwrap()
        .join("upgraded-worker");
    f.config.resume.executable = f.config.initial.executable.clone();
    let outcome = f.repair_outcome();
    assert!(outcome.skipped.is_empty() && outcome.held.is_empty());
    assert_eq!(outcome.launched.len(), 1);
    assert_eq!(outcome.launched[0].worker_build, original.worker_build);
    assert_eq!(
        outcome.launched[0].executable,
        original.worker_build.unwrap().path
    );
}

#[test]
fn pin_adoption_enables_legacy_pr_repair_after_pin_hold_and_restart() {
    let mut f = Fixture::new();
    let build = crate::pin_adoption_fixture::legacy(&f.config, &f.store);
    assert!(f.repair().is_err());
    let history = crate::pin_adoption_fixture::rows(&f.config);
    crate::pin_adoption_fixture::adopt(&mut f.store, &build);
    assert_eq!(crate::pin_adoption_fixture::rows(&f.config), history);
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    drop(f.store);
    f.store = StateStore::open(&f.config.state_root, 1).unwrap();
    let next = f.repair().unwrap().remove(0);
    assert_eq!(next.worker_build, Some(build.clone()));
    assert_eq!(next.executable, build.path);
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
}

mod head_advancement;

mod hold_diagnostics;
