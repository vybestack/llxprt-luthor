use crate::coordinator::pr_merge::{MergeError, MergeReport, merge_verified_prs};
use crate::{
    config::{Config, MergePolicy},
    github::pull_request::{LookupError, PrMerger},
    pr_merge::MergeEvidence,
    state::{StateStore, journal, task_records},
};
use crate::{
    eligibility::Candidate,
    github::pull_request::{PrHealth, PrHealthReader, PullRequestReader, RequiredChecksSource},
    model::VerifiedOpenPr,
};
use serde_json::{Value, json};

pub struct Fixture {
    pub store: StateStore,
    pub config: Config,
    pub prs: FakePrs,
    _dir: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        Self::with_login("acoliver")
    }

    pub fn with_login(login: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::from_json(&json!({
            "state_root":dir.path().join("state"), "worktree_root":dir.path().join("worktrees"), "capacity":1,
            "assignment_login":login, "sources":[{"project_id":"P", "repositories":["org/code"], "ready_marker":{"kind":"label","name":"ready"}}],
            "mappings":[{"tracker_repository":"org/code","code_repository":"org/code","checkout":dir.path().join("checkout"),"base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/code","allowed_pr_author":login}],
            "initial":{"executable":"/bin/fixture", "args":[]}, "resume":{"executable":"/bin/fixture","args":[]}
        }).to_string()).unwrap();
        let candidate: Candidate = serde_json::from_value(json!({
            "project_id":"P", "item_id":"I", "repository":"org/code", "issue_node_id":"ISSUE", "issue_number":40,
            "issue_url":"https://github.com/org/code/issues/40", "tracker_repo_id":"REPO", "observed_at_unix_secs":1,
            "observed_state":"open", "observed_assignees":[login], "observed_labels":["ready"], "observed_project_fields":[],
            "marker":config.sources[0].ready_marker, "mapping":config.mappings[0], "source":config.sources[0]
        })).unwrap();
        let mut store = StateStore::open(&config.state_root, 1).unwrap();
        task_records::create_task(&mut store, "task", &candidate, "r1", &config).unwrap();
        store.connection.execute_batch("UPDATE tasks SET state='pr_complete'; INSERT INTO attempts(id,task_id,lifecycle,outcome) VALUES('attempt','task','completed','exited');").unwrap();
        let saved: VerifiedOpenPr = serde_json::from_value(json!({
            "id":42,"number":7,"url":"https://github.com/org/code/pull/7","repository_id":10,"repository":"org/code",
            "head_repository_id":10,"head_repository":"org/code","base_branch":"main","head_branch":"luthor/task",
            "author":login,"active_login":login,"tracker_issue_url":candidate.issue_url,"draft":false,
            "checks":null,"created_at":"2026-10-01T00:00:00Z","head_commit_sha":"a".repeat(40),"observed_at":1,"attempt_id":"attempt"
        })).unwrap();
        seed_worktree(&mut store, &saved);
        journal::record_evidence(
            &mut store,
            "task",
            Some("attempt"),
            "verified_open_pr",
            &serde_json::to_string(&saved).unwrap(),
        )
        .unwrap();
        let prs = FakePrs::new(&saved);
        let mut config = config;
        config.mappings[0].merge = MergePolicy::Enabled {
            method: crate::config::MergeMethod::Squash,
            required_checks: crate::config::RequiredChecks::Rule(crate::config::CheckRule::All {}),
            allow_draft: false,
        };
        Self {
            store,
            config,
            prs,
            _dir: dir,
        }
    }

    pub fn run(&mut self) -> Result<MergeReport, MergeError> {
        merge_verified_prs(&mut self.store, &self.config, &mut self.prs)
    }

    /// A merge-step error never fails the sweep: the task is held with its
    /// reason and the error is journaled.
    pub fn held(&mut self) -> String {
        let report = self
            .run()
            .expect("a merge-step error must not fail the sweep");
        assert!(report.merged.is_empty() && report.delivered.is_empty());
        assert_eq!(report.waiting.len(), 1);
        assert!(
            journal::evidence_kinds(&self.store, "task")
                .unwrap()
                .contains(&"pr_merge_error".into())
        );
        report.waiting[0].reason.clone()
    }

    pub fn evidence(&self) -> MergeEvidence {
        let payload: String = self
            .store
            .connection
            .query_row(
                "SELECT payload FROM evidence WHERE kind='pr_merged'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&payload).unwrap()
    }
}

fn seed_worktree(store: &mut StateStore, saved: &VerifiedOpenPr) {
    let intent = json!({"path":"/fixture/worktree","branch":saved.head_branch,"base":"main","repository":"org/code"});
    let identity = json!({"path":"/fixture/worktree","device":1,"inode":2,"branch":saved.head_branch,"base":"main","head":saved.head_commit_sha,"repository":"org/code","git_directory":"/fixture/git","remote":"origin"});
    journal::record_intent(
        store,
        "wt",
        "task",
        None,
        "worktree_create",
        &intent.to_string(),
    )
    .unwrap();
    journal::record_evidence(
        store,
        "task",
        None,
        "worktree_created",
        &identity.to_string(),
    )
    .unwrap();
}

pub struct FakePrs {
    pub detail: Value,
    pub checks: crate::github::pull_request::MergeChecks,
    pub login: String,
    pub writes: Vec<String>,
    pub reads: usize,
    pub fail_write: bool,
    pub fail_terminal: bool,
    pub race: bool,
    pub tracker_closed: bool,
    pub tracker_writes: usize,
    pub fail_close: bool,
    pub fail_close_read: bool,
    pub updates: usize,
}

impl FakePrs {
    fn new(saved: &VerifiedOpenPr) -> Self {
        Self {
            detail: json!({"id":42,"number":7,"html_url":saved.url,"state":"open","merged":false,"draft":false,
                "mergeable":true,"mergeable_state":"clean","created_at":saved.created_at,
                "head":{"sha":saved.head_commit_sha,"repo":{"id":10,"full_name":"org/code"},"ref":saved.head_branch},
                "base":{"repo":{"id":10,"full_name":"org/code"},"ref":"main"},"user":{"login":saved.author},
                "body":format!("Tracker-Issue: {}\nFixes #40",saved.tracker_issue_url)}),
            checks: crate::github::pull_request::MergeChecks {
                health: PrHealth {
                    id: 42,
                    head: saved.head_commit_sha.clone(),
                    conflicting: false,
                    failed_required_checks: vec![],
                    required_checks_pending: false,
                    failed_jobs: vec![],
                    required_jobs: vec![],
                    required_checks_source: RequiredChecksSource::ConfiguredAll,
                },
                summary: vec![crate::github::pull_request::CheckSummary {
                    name: "CI".into(),
                    required: true,
                    state: "SUCCESS".into(),
                }],
            },
            login: saved.active_login.clone(),
            writes: vec![],
            reads: 0,
            fail_write: false,
            fail_terminal: false,
            race: false,
            tracker_closed: false,
            tracker_writes: 0,
            fail_close: false,
            fail_close_read: false,
            updates: 0,
        }
    }
    pub fn merged(&mut self) {
        self.detail["state"] = json!("closed");
        self.detail["merged"] = json!(true);
        self.detail["merged_at"] = json!("2026-10-14T12:00:00Z");
        self.detail["merge_commit_sha"] = json!("c".repeat(40));
    }
}

impl PullRequestReader for FakePrs {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok(self.login.clone())
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<Value>, LookupError> {
        panic!("no open PR search substitutes for saved PR")
    }
    fn repository_identity(&mut self, _: &str) -> Result<u64, LookupError> {
        Ok(10)
    }
    fn detail(&mut self, _: &str, _: u64) -> Result<Value, LookupError> {
        self.reads += 1;
        if self.fail_terminal && !self.writes.is_empty() {
            return Err(upstream());
        }
        Ok(self.detail.clone())
    }
}
impl PrHealthReader for FakePrs {
    fn rerun_failed_jobs(&mut self, _: &str, _: u64, _: &str) -> Result<(), LookupError> {
        panic!("merge cannot rerun jobs")
    }
    fn failed_job_logs(&mut self, _: &str, _: u64) -> Result<String, LookupError> {
        panic!("merge cannot launch repairs")
    }
    fn health(
        &mut self,
        _: &str,
        _: u64,
        _: &crate::config::RequiredChecks,
    ) -> Result<PrHealth, LookupError> {
        panic!("strict merge checks required")
    }
}
impl PrMerger for FakePrs {
    fn merge_checks(
        &mut self,
        _: &str,
        _: u64,
        _: &crate::config::RequiredChecks,
    ) -> Result<crate::github::pull_request::MergeChecks, LookupError> {
        if self.race {
            self.detail["head"]["sha"] = json!("b".repeat(40));
        }
        Ok(self.checks.clone())
    }
    fn tracker_issue(
        &mut self,
        target: &crate::pr_merge::TrackerTarget,
    ) -> Result<crate::pr_merge::TrackerIssue, LookupError> {
        if self.fail_close_read && self.tracker_writes > 0 {
            return Err(upstream());
        }
        Ok(crate::pr_merge::TrackerIssue {
            target: target.clone(),
            closed_at: self.tracker_closed.then(|| "2026-10-14T12:00:00Z".into()),
        })
    }
    fn close_tracker(
        &mut self,
        _: &crate::pr_merge::MergeEvidence,
        _: &crate::pr_merge::TrackerTarget,
    ) -> Result<(), LookupError> {
        self.tracker_writes += 1;
        if self.fail_close {
            return Err(upstream());
        }
        self.tracker_closed = true;
        Ok(())
    }
    fn update_branch(&mut self, _: &VerifiedOpenPr) -> Result<(), LookupError> {
        self.updates += 1;
        if self.fail_write {
            return Err(upstream());
        }
        self.detail["head"]["sha"] = json!("b".repeat(40));
        self.detail["mergeable_state"] = json!("unknown");
        Ok(())
    }
    fn ready_for_review(&mut self, _: &VerifiedOpenPr) -> Result<(), LookupError> {
        self.writes.push("ready".into());
        self.detail["draft"] = json!(false);
        self.detail["mergeable_state"] = json!("clean");
        Ok(())
    }
    fn merge_head(
        &mut self,
        pr: &VerifiedOpenPr,
        method: crate::config::MergeMethod,
    ) -> Result<String, LookupError> {
        self.writes
            .push(format!("{}:{}", method.as_str(), pr.head_commit_sha));
        if self.fail_write {
            return Err(upstream());
        }
        self.merged();
        Ok("c".repeat(40))
    }
}
fn upstream() -> LookupError {
    LookupError {
        category: crate::github::pull_request::ErrorCategory::Transport,
        code: "fixture-unavailable",
        status: None,
    }
}
