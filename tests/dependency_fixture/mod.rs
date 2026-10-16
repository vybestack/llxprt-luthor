use luthor::{config::Config, github::project::GhProjectReader, state::StateStore};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

pub struct Fixture {
    dir: tempfile::TempDir,
    pub config: Config,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let config = Config::from_json(
            &json!({
                "state_root":root.join("state"), "worktree_root":root.join("worktrees"),
                "capacity":2, "assignment_login":"acoliver",
                "sources":[{"project_id":"PROJECT","repositories":["org/tracker"],
                    "ready_marker":{"kind":"label","name":"ready"},"milestone":null,
                    "dependencies":[{"issue":{"repository":"org/tracker","number":2},
                        "blocked_by":[{"repository":"org/tracker","number":1}]}]}],
                "mappings":[{"tracker_repository":"org/tracker","code_repository":"org/tracker",
                    "checkout":root.join("checkout"),"base_branch":"main",
                    "push_remote":"git@github.com:org/tracker.git",
                    "allowed_pr_head_repository":"org/tracker","allowed_pr_author":"acoliver"}],
                "initial":{"executable":"/bin/echo","args":["initial"]},
                "resume":{"executable":"/bin/echo","args":["resume"]}
            })
            .to_string(),
        )
        .unwrap();
        fs::create_dir(root.join("worktrees")).unwrap();
        drop(StateStore::open(&config.state_root, 2).unwrap());
        let f = Self { dir, config };
        f.write("config", &serde_json::to_value(&f.config).unwrap());
        f.write(
            "quota",
            &json!({"resources":{
                "core":{"remaining":5000,"reset":9999999999_u64},
                "graphql":{"remaining":5000,"reset":9999999999_u64}
            }}),
        );
        f.write("repository", &json!({"node_id":"REPO"}));
        f.write(
            "page",
            &json!({"data":{"node":{"items":{
                "nodes":[item(2),item(3)],
                "pageInfo":{"hasNextPage":false,"endCursor":null}
            }}}}),
        );
        f.write("issue2", &issue("org/tracker", 2));
        f.write("issue3", &issue("org/tracker", 3));
        f.write("cross-dependency", &issue("other/foundation", 1));
        f.set_state("open", false, false);
        fs::write(
            f.dir.path().join("gh"),
            format!(
                r##"#!/bin/sh
set -eu
cd '{}'
printf '%s\n' "$*" >> invocations.log
case "$1 $2" in
  'api rate_limit') file=quota ;;
  'api graphql') file=page ;;
  'api repos/org/tracker') file=repository ;;
  'api repos/org/tracker/issues/2?per_page=100') file=issue2 ;;
  'api repos/org/tracker/issues/3?per_page=100') file=issue3 ;;
  'api repos/org/tracker/issues/1'|'api repos/org/tracker/issues/1?per_page=100') file=dependency ;;
  'api repos/other/foundation/issues/1') file=cross-dependency ;;
  'api repos/org/tracker/pulls/1') file=pull ;;
  *) echo 'unexpected command' >&2; exit 1 ;;
esac
cat "$file.json"
if grep -q 'Forbidden' "$file.json"; then exit 1; fi
"##,
                f.dir.path().display()
            ),
        )
        .unwrap();
        fs::set_permissions(f.dir.path().join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
        f
    }

    pub fn reader(&self) -> GhProjectReader {
        GhProjectReader::new(self.dir.path().join("gh"))
    }

    pub fn read(&self, file: &str) -> Value {
        serde_json::from_slice(&fs::read(self.dir.path().join(format!("{file}.json"))).unwrap())
            .unwrap()
    }

    pub fn write(&self, file: &str, value: &Value) {
        fs::write(
            self.dir.path().join(format!("{file}.json")),
            value.to_string(),
        )
        .unwrap();
    }

    pub fn set_state(&self, state: &str, is_pr: bool, merged: bool) {
        let mut dependency = issue("org/tracker", 1);
        dependency["state"] = json!(state);
        if is_pr {
            dependency["pull_request"] =
                json!({"url":"https://api.github.com/repos/org/tracker/pulls/1"});
            dependency["html_url"] = json!("https://github.com/org/tracker/pull/1");
        }
        self.write("dependency", &dependency);
        self.write(
            "pull",
            &json!({"node_id":"ISSUE1","number":1,"state":state,
            "html_url":"https://github.com/org/tracker/pull/1","merged":merged}),
        );
    }

    pub fn run(&self, args: &[&str]) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_luthor"))
            .args(args)
            .arg("--config")
            .arg(self.dir.path().join("config.json"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir.path().display()),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    pub fn assert_read_only(&self) {
        let log = fs::read_to_string(self.dir.path().join("invocations.log")).unwrap();
        assert!(!log.contains("-X"));
        // The project query reads assignees; assignment endpoints remain forbidden.
        assert!(
            log.lines()
                .all(|line| { line.starts_with("api graphql ") || !line.contains("assignees") })
        );
        assert!(!log.contains("initial"));
        assert!(!log.contains("resume"));
        assert!(!log.contains("git "));
    }
}

fn issue(repository: &str, number: u64) -> Value {
    json!({"node_id":format!("ISSUE{number}"), "number":number,
        "html_url":format!("https://github.com/{repository}/issues/{number}"),
        "repository_url":format!("https://api.github.com/repos/{repository}"),
        "state":"open", "assignees":[],"labels":[{"name":"ready"}],"milestone":null})
}

fn item(number: u64) -> Value {
    json!({"id":format!("ITEM{number}"),"content":{"__typename":"Issue","state":"OPEN","labels":{"nodes":[{"name":"ready"}],"pageInfo":{"hasNextPage":false}},"assignees":{"nodes":[],"pageInfo":{"hasNextPage":false}},"milestone":null,
        "id":format!("ISSUE{number}"),"number":number,
        "repository":{"id":"REPO","nameWithOwner":"org/tracker"}},
        "fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}})
}
