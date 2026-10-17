use super::*;
#[test]
fn native_continuation_refuses_wrong_mode_claim_source_identity_and_present_or_ambiguous_pr() {
    let mut lane = fixture(false);
    for scenario in [
        "unassigned",
        "extra_assignee",
        "project_identity",
        "issue_identity",
        "closed",
        "not_ready",
        "identity_mismatch",
        "ambiguous",
        "source_error",
        "pr_error",
        "present",
    ] {
        lane.github.prs = crate::FakePr::default();
        if scenario == "present" {
            lane.github.prs.present_on = Some(1);
        }
        if scenario == "pr_error" {
            lane.github.prs.fail_on = Some(lane.github.prs.lookups + 1);
        }
        let mut launcher = Launcher {
            count: 0,
            execute: true,
        };
        assert!(
            run(
                &mut lane,
                Mode::NaturalExitRetry,
                "attempt-task-a",
                "attempt-second",
                scenario,
                &mut launcher
            )
            .is_err(),
            "{scenario}"
        );
        assert_eq!(launcher.count, 0, "{scenario}");
        assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
        assert_eq!(
            task_records::latest_attempt(&lane.f.store, "task-a")
                .unwrap()
                .as_deref(),
            Some("attempt-task-a")
        );
    }
    for (stopped, mode) in [(false, Mode::StoppedResume), (true, Mode::NaturalExitRetry)] {
        let mut lane = fixture(stopped);
        let mut launcher = Launcher {
            count: 0,
            execute: true,
        };
        assert!(
            run(
                &mut lane,
                mode,
                "attempt-task-a",
                "attempt-second",
                "ok",
                &mut launcher
            )
            .is_err()
        );
        assert_eq!(launcher.count, 0);
    }
}
#[test]
fn native_continuation_uncertain_dispatch_retains_slot_and_never_replays() {
    let mut lane = fixture(false);
    let mut launcher = Launcher {
        count: 0,
        execute: false,
    };
    assert!(
        run(
            &mut lane,
            Mode::NaturalExitRetry,
            "attempt-task-a",
            "attempt-second",
            "ok",
            &mut launcher
        )
        .is_err()
    );
    assert_eq!(launcher.count, 1);
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
    assert!(
        run(
            &mut lane,
            Mode::NaturalExitRetry,
            "attempt-task-a",
            "attempt-third",
            "ok",
            &mut launcher
        )
        .is_err()
    );
    assert_eq!(launcher.count, 1);
}
