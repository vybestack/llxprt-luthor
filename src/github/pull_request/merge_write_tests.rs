#![cfg(unix)]
use super::{GhPullRequestReader, PrMerger};
use crate::{config::MergeMethod, model::VerifiedOpenPr};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

struct Fixture {
    reader: GhPullRequestReader,
    saved: VerifiedOpenPr,
    calls: PathBuf,
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new(login: &str, fail: bool, race: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let saved: VerifiedOpenPr = serde_json::from_value(json!({
            "id":42,"number":7,"url":"https://github.com/org/code/pull/7","repository_id":10,"repository":"org/code",
            "head_repository_id":10,"head_repository":"org/code","base_branch":"main","head_branch":"luthor/task",
            "author":"acoliver","active_login":"acoliver","tracker_issue_url":"https://github.com/org/code/issues/40","draft":false,
            "checks":null,"created_at":"2026-10-01T00:00:00Z","head_commit_sha":"a".repeat(40),"observed_at":1,"attempt_id":"attempt"
        })).unwrap();
        let detail = json!({"id":42,"number":7,"html_url":saved.url,"state":"open","merged":false,"draft":false,
            "mergeable":true,"mergeable_state":"clean","created_at":saved.created_at,
            "head":{"sha":saved.head_commit_sha,"repo":{"id":10,"full_name":"org/code"},"ref":saved.head_branch},
            "base":{"repo":{"id":10,"full_name":"org/code"},"ref":"main"},"user":{"login":"acoliver"},
            "body":format!("Tracker-Issue: {}\nFixes #40",saved.tracker_issue_url)});
        fs::write(dir.path().join("detail"), detail.to_string()).unwrap();
        let mut changed = detail;
        changed["head"]["sha"] = json!("b".repeat(40));
        fs::write(dir.path().join("changed"), changed.to_string()).unwrap();
        let calls = dir.path().join("calls");
        let script = dir.path().join("gh");
        fs::write(&script, format!(r#"#!/bin/sh
printf '%s\n' "$*" >> '{calls}'
case "$1:$2" in
api:user) printf '%s\n' '{login}' ;;
api:repos/org/code/pulls/7)
 if test -f '{root}/read' && test '{race}' = true; then cat '{root}/changed'; else cat '{root}/detail'; fi
 touch '{root}/read' ;;
api:--method)
 test "$3" = PUT && test "$4" = repos/org/code/pulls/7/merge || exit 91
 test "$5" = -f && test "$6" = sha={head} || exit 92
 if test '{fail}' = true; then exit 1; fi
 printf '%s\n' '{{"merged":true,"sha":"{commit}"}}' ;;
pr:ready) printf '%s\n' 'ready' ;;
*) exit 99 ;;
esac
"#, calls=calls.display(),root=dir.path().display(),head="a".repeat(40),commit="c".repeat(40))).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            reader: GhPullRequestReader::new(script),
            saved,
            calls,
            dir,
        }
    }
    fn calls(&self) -> String {
        fs::read_to_string(&self.calls).unwrap()
    }
}

#[test]
fn merge_write_uses_exact_saved_sha_and_selected_method_without_admin_or_auto() {
    for method in [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase] {
        let mut f = Fixture::new("acoliver", false, false);
        assert_eq!(
            f.reader.merge_head(&f.saved, method).unwrap(),
            "c".repeat(40)
        );
        let calls = f.calls();
        assert_eq!(
            calls
                .lines()
                .filter(|call| call.contains("--method PUT"))
                .count(),
            1
        );
        assert!(calls.contains(&format!(
            "sha={} -f merge_method={}",
            "a".repeat(40),
            method.as_str()
        )));
        assert!(!calls.contains("admin") && !calls.contains("auto"));
    }
}

#[test]
fn merge_write_rechecks_account_and_head_and_never_retries_mutation_failure() {
    for (login, fail, race, expected_writes) in [
        ("other", false, false, 0),
        ("acoliver", false, true, 0),
        ("acoliver", true, false, 1),
    ] {
        let mut f = Fixture::new(login, fail, race);
        assert!(f.reader.merge_head(&f.saved, MergeMethod::Squash).is_err());
        assert_eq!(
            f.calls()
                .lines()
                .filter(|call| call.contains("--method PUT"))
                .count(),
            expected_writes
        );
    }
}

#[test]
fn malformed_merge_response_and_changed_clean_state_do_not_become_success() {
    let mut f = Fixture::new("acoliver", false, false);
    let mut detail: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(f.dir.path().join("detail")).unwrap()).unwrap();
    detail["mergeable_state"] = json!("unstable");
    fs::write(f.dir.path().join("detail"), detail.to_string()).unwrap();
    assert!(f.reader.merge_head(&f.saved, MergeMethod::Squash).is_err());
    assert!(!f.calls().contains("--method PUT"));
}

#[test]
fn malformed_and_refused_merge_responses_are_not_swallowed() {
    for response in [
        r#"{"merged":false,"sha":"cccccccccccccccccccccccccccccccccccccccc"}"#,
        r#"{"merged":true,"sha":"bad"}"#,
        "not json",
    ] {
        let mut f = Fixture::new("acoliver", false, false);
        let script = f.dir.path().join("gh");
        let original = fs::read_to_string(&script).unwrap();
        let good = format!(r#"{{"merged":true,"sha":"{}"}}"#, "c".repeat(40));
        assert!(original.contains(&good));
        fs::write(&script, original.replace(&good, response)).unwrap();
        assert!(f.reader.merge_head(&f.saved, MergeMethod::Squash).is_err());
        assert_eq!(
            f.calls()
                .lines()
                .filter(|call| call.contains("--method PUT"))
                .count(),
            1
        );
    }
}

#[test]
fn gh_merge_keeps_existing_acoliver_identity_restriction_in_addition_to_mapping_author() {
    let mut f = Fixture::new("release-bot", false, false);
    f.saved.active_login = "release-bot".into();
    f.saved.author = "release-bot".into();
    let file = f.dir.path().join("detail");
    let mut detail: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    detail["user"]["login"] = json!("release-bot");
    fs::write(file, detail.to_string()).unwrap();
    assert!(f.reader.merge_head(&f.saved, MergeMethod::Squash).is_err());
    assert!(!f.calls().contains("--method PUT"));
}

mod followup;
