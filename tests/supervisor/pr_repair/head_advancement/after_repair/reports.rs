//! The repair artifact is not a blanket dirty-worktree exemption.
use super::*;

#[test]
fn base_update_after_repair_preserves_dirty_or_unverified_work() {
    for scenario in [
        "dirty",
        "stale",
        "malformed",
        "semantic",
        "symlink",
        "tracked",
        "no_completion",
        "oversized",
        "directory",
        "unknown_field",
        "ignored_stale",
    ] {
        let mut f = completed_repair();
        let old = f.prs.head.clone();
        change_report(&f, scenario);
        let before = read_git(
            &local(&f),
            &["status", "--porcelain=v1", "--untracked-files=all"],
        );
        advance(&mut f, "base_merge");
        assert_eq!(f.repair_outcome().held.len(), 1, "{scenario}");
        assert!(restore(&mut f).restored.is_empty(), "{scenario}");
        assert_eq!(
            read_git(&local(&f), &["rev-parse", "HEAD"]),
            old,
            "{scenario}"
        );
        assert_eq!(
            read_git(
                &local(&f),
                &["status", "--porcelain=v1", "--untracked-files=all"]
            ),
            before,
            "{scenario}"
        );
        if scenario == "dirty" {
            assert_eq!(
                fs::read_to_string(local(&f).join("keep-me")).unwrap(),
                "operator work"
            );
        }
    }
}

fn change_report(f: &Fixture, scenario: &str) {
    let report = local(f).join(REPAIR_RESULT_FILE);
    match scenario {
        "dirty" => fs::write(local(f).join("keep-me"), "operator work").unwrap(),
        "ignored_stale" => {
            ignore_report(f);
            fs::write(
                &report,
                r#"{"attempt_id":"attempt-real","status":"repaired"}"#,
            )
            .unwrap();
        }
        "stale" => fs::write(
            &report,
            r#"{"attempt_id":"attempt-real","status":"repaired"}"#,
        )
        .unwrap(),
        "malformed" => fs::write(&report, "not json").unwrap(),
        "semantic" => {
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
            value["status"] = serde_json::json!("semantic_conflict");
            fs::write(&report, value.to_string()).unwrap();
        }
        "symlink" => {
            fs::remove_file(&report).unwrap();
            std::os::unix::fs::symlink("base-change", &report).unwrap();
        }
        "oversized" => fs::write(&report, " ".repeat(4097)).unwrap(),
        "directory" => {
            fs::remove_file(&report).unwrap();
            fs::create_dir(&report).unwrap();
        }
        "unknown_field" => {
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
            value["unexpected"] = serde_json::json!(true);
            fs::write(&report, value.to_string()).unwrap();
        }
        "tracked" => git(&local(f), &["add", REPAIR_RESULT_FILE]),
        "no_completion" => {
            let db = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
            db.execute(
                "DELETE FROM evidence WHERE kind='verified_open_pr' AND attempt_id!='attempt-real'",
                [],
            )
            .unwrap();
        }
        _ => unreachable!(),
    }
}
