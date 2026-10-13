use super::{Config, Duration, ExitTiming, Instant, PathBuf, await_exit, journal, thread};
use crate::{fixture, tempdir};
use luthor::state::StateStore;
use luthor::state::{launches, worktree_records};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

pub(super) struct Fixture {
    pub root: tempfile::TempDir,
    pub config: Config,
    config_path: PathBuf,
    search_path: String,
}

impl Fixture {
    pub fn new() -> Self {
        let root = tempdir();
        let checkout = root.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.org"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["config", "core.hooksPath", "/dev/null"],
            vec!["remote", "add", "origin", "git@github.com:org/code.git"],
            vec!["commit", "--allow-empty", "-m", "base"],
        ] {
            git(&checkout, &args);
        }
        crate::git_fixture::remote(&checkout);
        let ssh = root.path().join("remote.git/fixture-ssh");
        fs::write(
            &ssh,
            format!(
                r#"#!/bin/sh
set -eu
for arg do
  case "$arg" in
    git-upload-pack*) exec git-upload-pack '{}' ;;
    git-receive-pack*) exec git-receive-pack '{}' ;;
  esac
done
exit 99
"#,
                root.path().join("remote.git").display(),
                root.path().join("remote.git").display()
            ),
        )
        .unwrap();
        assert!(
            Command::new("sh")
                .arg("-n")
                .arg(&ssh)
                .status()
                .unwrap()
                .success()
        );
        let (config_path, search_path) = fixture(root.path(), "acoliver");
        let worker = root.path().join("fixture-worker");
        fs::write(&worker, worker_script(root.path())).unwrap();
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
        let mut configured: Value =
            serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        let command = |prompt| json!({"executable":worker,"args":["--session","{task.id}","--cwd","{worktree}","--attempt","{attempt.id}","-p",prompt]});
        configured["initial"] = command("Start issue");
        configured["resume"] = command("Continue issue");
        fs::write(&config_path, configured.to_string()).unwrap();
        let config = Config::from_json(&configured.to_string()).unwrap();
        install_gh(root.path());
        Self {
            root,
            config,
            config_path: config_path.into(),
            search_path,
        }
    }

    pub fn command(&self, command: &str, args: &[&str]) -> Value {
        let mut arguments: Vec<String> = args.iter().map(|arg| (*arg).into()).collect();
        let config_index = if command == "retry" {
            3
        } else {
            arguments.len()
        };
        arguments.splice(
            config_index..config_index,
            [
                "--config".into(),
                self.config_path.to_string_lossy().into_owned(),
            ],
        );
        let output = Command::new(env!("CARGO_BIN_EXE_luthor"))
            .arg(command)
            .args(arguments)
            .env("PATH", &self.search_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout={}\nstderr={}\nstate={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            self.state_evidence()
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    pub fn state_evidence(&self) -> String {
        let db = rusqlite::Connection::open(self.config.state_root.join("state.sqlite3")).unwrap();
        let mut statement = db
            .prepare("SELECT kind,payload FROM evidence ORDER BY sequence")
            .unwrap();
        format!(
            "{:?}",
            statement
                .query_map([], |row| Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?
                )))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        )
    }

    pub fn dispatch(&self) -> String {
        self.command(
            "dispatch",
            &[
                "--repository",
                "org/tracker",
                "--issue",
                "7",
                "--config-revision",
                "initial",
                "--execute",
            ],
        )["task_id"]
            .as_str()
            .unwrap()
            .into()
    }

    pub fn cycle(&self) -> Value {
        self.command(
            "daemon",
            &[
                "--config-revision",
                "cycle",
                "--repository",
                "org/tracker",
                "--issues",
                "7",
                "--once",
                "--execute",
            ],
        )
    }

    pub fn enable_worker(&self) {
        fs::write(self.root.path().join("enabled"), "enabled").unwrap();
    }

    pub fn publish_pr(&self, task: &str) {
        let store = StateStore::open(&self.config.state_root, 1).unwrap();
        let identity = worktree_records::worktree_record(&store, task)
            .unwrap()
            .unwrap()
            .identity
            .unwrap();
        let detail = json!({"id":4242,"number":42,"state":"open","draft":false,"merged":false,
            "html_url":"https://github.com/org/code/pull/42",
            "body":"Tracker-Issue: https://github.com/org/tracker/issues/7\nFixes org/tracker#7",
            "created_at":"2026-01-01T00:00:00Z",
            "base":{"repo":{"id":10,"full_name":"org/code"},"ref":"main"},
            "head":{"repo":{"id":10,"full_name":"org/code"},"ref":identity.branch,"sha":identity.head},
            "user":{"login":"acoliver"}});
        fs::write(self.root.path().join("pr.json"), detail.to_string()).unwrap();
        fs::write(self.root.path().join("head"), identity.head).unwrap();
    }

    pub fn finish_during_cycle(&self, task: &str, attempt: &str, timing: ExitTiming) {
        let store = StateStore::open(&self.config.state_root, 1).unwrap();
        let plan: luthor::supervisor::LaunchPlan =
            serde_json::from_str(&launches::launch_intent(&store, attempt).unwrap().unwrap())
                .unwrap();
        let receipt = std::env::current_dir()
            .unwrap()
            .join(&self.config.state_root)
            .join(format!("attempts/{attempt}.receipt.json"));
        let ready: Value = serde_json::from_str(
            &journal::evidence_payloads(&store, task, attempt, "supervisor_ready").unwrap()[0],
        )
        .unwrap();
        fs::write(self.root.path().join("receipt"), receipt.to_str().unwrap()).unwrap();
        fs::write(
            self.root.path().join("supervisor-pid"),
            ready["pid"].to_string(),
        )
        .unwrap();
        drop(store);
        // The worker has started and is gated on our release file, not a sleep.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.root.path().join("started").exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        if !matches!(timing, ExitTiming::BeforeCycle) {
            fs::write(self.root.path().join("race"), format!("{timing:?}")).unwrap();
        } else {
            fs::write(self.root.path().join("release"), "release").unwrap();
            await_exit(self, task, attempt);
        }
        assert_eq!(plan.task_id, task);
    }
}

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn worker_script(root: &Path) -> String {
    format!(
        r#"#!/bin/sh
set -eu
root='{}'
if ! test -f "$root/enabled"; then exit 2; fi
attempt=''
while test "$#" -gt 0; do
  if test "$1" = --attempt; then attempt="$2"; shift; fi
  shift
done
touch "$root/started"
i=0
while ! test -f "$root/release"; do
  i=$((i+1)); test "$i" -lt 1000 || exit 98
  sleep 0.01
done
git commit --allow-empty -m 'worker push'
git push origin HEAD
head=$(git rev-parse HEAD)
branch=$(git branch --show-current)
printf '%s' "$head" > "$root/head"
printf '{{"id":4242,"number":42,"state":"open","draft":false,"merged":false,"html_url":"https://github.com/org/code/pull/42","body":"Tracker-Issue: https://github.com/org/tracker/issues/7\\nFixes org/tracker#7","created_at":"2026-01-01T00:00:00Z","base":{{"repo":{{"id":10,"full_name":"org/code"}},"ref":"main"}},"head":{{"repo":{{"id":10,"full_name":"org/code"}},"ref":"%s","sha":"%s"}},"user":{{"login":"acoliver"}}}}' "$branch" "$head" > "$root/pr.json"
printf '{{"attempt_id":"%s","status":"repaired"}}' "$attempt" > .luthor-pr-repair-result.json
rm -f "$root/conflict"
if test -f "$root/finish-wait"; then
  sleep 1
  i=0
  while ! test -f "$root/finish-release"; do
    i=$((i+1)); test "$i" -lt 6000 || exit 98
    sleep 0.01
  done
fi
printf '{{"summary":"worker finished"}}\n'
"#,
        root.display()
    )
}

fn install_gh(root: &Path) {
    let gh = root.join("gh");
    let original = fs::read_to_string(&gh).unwrap();
    let routes = format!(
        r#"  repos/org/code/pulls/42) cat '{}/pr.json' ;;
  repos/org/code/pulls/43) sed 's/"number":42/"number":43/g; s@pull/42@pull/43@g' '{}/pr.json' ;;
  repos/org/code/pulls?*)
    root='{}'
    if test -f "$root/race"; then
      timing=$(cat "$root/race")
      rm "$root/race"
      touch "$root/release"
      i=0
      while ! test -f "$(cat "$root/receipt")" || kill -0 "$(cat "$root/supervisor-pid")" 2>/dev/null; do
        i=$((i+1)); test "$i" -lt 1000 || exit 98
        sleep 0.01
      done
      # Force an active-stage observation to finish after the worker exits.
      # Neither absence nor presence may replace a fresh completion lookup.
      if test "$timing" = DuringAbsentLookup; then
        printf '%s\n' '[]'
        exit 0
      fi
    fi
    if test -f "$root/ambiguous"; then
      printf '%s\n' '[{{"number":42,"body":"Tracker-Issue: https://github.com/org/tracker/issues/7"}},{{"number":43,"body":"Tracker-Issue: https://github.com/org/tracker/issues/7"}}]'
    elif test -f "$root/pr.json"; then
      printf '%s\n' '[{{"number":42,"body":"Tracker-Issue: https://github.com/org/tracker/issues/7\nFixes org/tracker#7"}}]'
    else printf '%s\n' '[]'; fi ;;
  repos/org/code) printf '%s\n' '{{"id":10,"full_name":"org/code"}}' ;;
"#,
        root.display(),
        root.display(),
        root.display()
    );
    let health = format!(
        r#"  graphql)
    case "$*" in
      *'number=42'*)
        head=$(cat '{}/head')
        mergeable=MERGEABLE
        if test -f '{}/conflict'; then mergeable=CONFLICTING; fi
        printf '{{"data":{{"repository":{{"pullRequest":{{"databaseId":4242,"headRefOid":"%s","mergeable":"%s","commits":{{"nodes":[{{"commit":{{"oid":"%s","statusCheckRollup":null}}}}]}}}}}}}}}}' "$head" "$mergeable" "$head" ;;
      *) printf '%s\n' '{}' ;;
    esac ;;
"#,
        root.display(),
        root.display(),
        project_payload()
    );
    let project_route = original
        .lines()
        .find(|line| line.trim_start().starts_with("graphql)"))
        .unwrap();
    let script = original
        .replace("  repos/org/code/pulls?*) printf '%s\\n' '[]' ;;", &routes)
        .replace(project_route, &health);
    assert!(script.contains("supervisor-pid"));
    fs::write(&gh, script).unwrap();
    assert!(
        Command::new("sh")
            .arg("-n")
            .arg(&gh)
            .status()
            .unwrap()
            .success()
    );
}

fn project_payload() -> Value {
    json!({"data":{"node":{"items":{"nodes":[{"id":"ITEM","content":{"__typename":"Issue","id":"ISSUE_NODE","number":7,"repository":{"id":"REPO_NODE","nameWithOwner":"org/tracker"}},"fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}})
}
