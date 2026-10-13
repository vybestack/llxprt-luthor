use super::*;
use luthor::state::{task_records, worktree_records};
use luthor::{cleanup::clean_terminal_tasks, config::CleanupPolicy};
use serde_json::{Value, json};

pub(super) struct TerminalPr {
    pub(super) detail: Value,
    pub(super) reads: usize,
    pub(super) replacement: bool,
    pub(super) fail: bool,
    pub(super) closed: bool,
}
impl PullRequestReader for TerminalPr {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok("operator".into())
    }
    fn repository_identity(&mut self, _: &str) -> Result<u64, LookupError> {
        Ok(10)
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<Value>, LookupError> {
        if !self.closed || self.replacement {
            Ok(vec![json!({"number":42,"body":self.detail["body"]})])
        } else {
            Ok(vec![])
        }
    }
    fn detail(&mut self, _: &str, _: u64) -> Result<Value, LookupError> {
        self.reads += 1;
        if self.fail {
            return Err(LookupError {
                category: ErrorCategory::Transport,
                code: "offline",
                status: None,
            });
        }
        let mut detail = self.detail.clone();
        if self.replacement {
            detail["state"] = json!("open");
        }
        Ok(detail)
    }
}

pub(super) fn fixture() -> (tempfile::TempDir, Config, StateStore, TerminalPr) {
    let (dir, mut config, mut store) = crate::supervisor_support::stop_views::completion_fixture();
    let identity = worktree_records::worktree_record(&store, "task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    fs::write(identity.path.join(".gitignore"), "target/\ntmp/\n").unwrap();
    git(&identity.path, &["add", ".gitignore"]);
    git(&identity.path, &["commit", "-m", "ignore build outputs"]);
    let head = Command::new("git")
        .arg("-C")
        .arg(&identity.path)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(head.status.success());
    let sha = String::from_utf8(head.stdout).unwrap().trim().to_owned();
    let mut prs = TerminalPr {
        detail: json!({
            "id":4242,"number":42,"state":"open","merged":false,
            "html_url":"https://github.com/org/code/pull/42",
            "body":"Tracker-Issue: https://github.com/org/tracker/issues/7",
            "draft":false,"created_at":"2026-01-01T00:00:00Z",
            "base":{"repo":{"id":10,"full_name":"org/code"},"ref":"main"},
            "head":{"repo":{"id":10,"full_name":"org/code"},"ref":identity.branch,"sha":sha},
            "user":{"login":"operator"},"closed_at":null,"merged_at":null
        }),
        reads: 0,
        replacement: false,
        fail: false,
        closed: false,
    };
    let candidate = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap()
        .candidate;
    let result = luthor::coordinator::reconcile_with_pr(
        &mut store,
        "task",
        "attempt-real",
        &mut OtherProject(candidate, 1),
        &mut prs,
    )
    .unwrap();
    assert!(
        matches!(result, Reconciliation::Completed { .. }),
        "{result:?}"
    );
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("pr_complete")
    );
    config.cleanup = CleanupPolicy::Artifacts {
        paths: vec!["target".into(), "tmp/build".into()],
    };
    fs::create_dir_all(identity.path.join("target/debug")).unwrap();
    fs::write(
        identity.path.join("target/debug/artifact"),
        "large build output",
    )
    .unwrap();
    fs::create_dir_all(identity.path.join("tmp/build")).unwrap();
    fs::write(identity.path.join("tmp/build/output"), "other output").unwrap();
    (dir, config, store, prs)
}

pub(super) fn close(prs: &mut TerminalPr, merged: bool) {
    prs.closed = true;
    prs.detail["state"] = json!("closed");
    prs.detail["merged"] = json!(merged);
    prs.detail["closed_at"] = json!("2026-10-01T00:00:00Z");
    prs.detail["merged_at"] = if merged {
        json!("2026-10-01T00:00:00Z")
    } else {
        Value::Null
    };
}

pub(super) fn root(config: &Config) -> std::path::PathBuf {
    config.worktree_root.join("task")
}

pub(super) fn clean(
    store: &mut StateStore,
    config: &Config,
    prs: &mut TerminalPr,
) -> luthor::cleanup::CleanupReport {
    clean_terminal_tasks(store, config, prs).unwrap()
}
