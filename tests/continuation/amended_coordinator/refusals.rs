use super::{Case, Refusal, amendment};
use luthor::coordinator::ProcessInspectionError;

#[test]
fn amended_coordinator_config_is_exact_correction_and_bounded_current_revision() {
    for mutation in [
        "unchanged",
        "initial",
        "resume",
        "mapping",
        "source",
        "capacity",
        "root",
        "assignment",
        "blank",
        "newline",
        "long",
    ] {
        let mut case = Case::new();
        match mutation {
            "unchanged" => {
                case.lane
                    .f
                    .config
                    .initial
                    .args
                    .splice(2..2, ["--branch".into(), "luthor/{task.id}".into()]);
            }
            "initial" => case.lane.f.config.initial.args.push("different".into()),
            "resume" => case.lane.f.config.resume.args.push("different".into()),
            "mapping" => case.lane.f.config.mappings[0].base_branch = "other".into(),
            "source" => case.lane.f.config.sources[0].milestone = None,
            "capacity" => case.lane.f.config.capacity = 2,
            "root" => case.lane.f.config.state_root = "other".into(),
            "assignment" => case.lane.f.config.assignment_login = "other".into(),
            "blank" => case.revision = " ",
            "newline" => case.revision = "corrected\nrevision",
            "long" => {
                case.revision = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
            }
            _ => unreachable!(),
        }
        case.held(Refusal::ConfigChanged, 0);
        assert_eq!(case.lane.local.inspections, 0);
    }
}

#[test]
fn amended_coordinator_external_failures_before_audit_keep_slot() {
    for (scenario, reason) in [
        ("identity_mismatch", Refusal::ActorMismatch),
        ("identity_error", Refusal::IdentityUnavailable),
        ("ambiguous", Refusal::PrPresent),
        ("source_error", Refusal::SourceUnavailable),
        ("unassigned", Refusal::ClaimChanged),
        ("extra_assignee", Refusal::ClaimChanged),
        ("project_identity", Refusal::ClaimChanged),
    ] {
        let mut case = Case::new();
        case.lane.read_scenario = scenario;
        case.held(reason, 0);
    }
    for reason in [
        Refusal::WorktreeChanged,
        Refusal::ArtifactConflict,
        Refusal::StorageUnavailable,
        Refusal::EnvironmentUnavailable,
    ] {
        let mut case = Case::new();
        case.lane.local.fail = Some(reason);
        case.held(reason, 0);
    }
    for (failure, reason) in [
        (ProcessInspectionError::Conflict, Refusal::ProcessConflict),
        (
            ProcessInspectionError::Unavailable,
            Refusal::ProcessUnavailable,
        ),
    ] {
        let mut case = Case::new();
        case.lane.processes.failure = Some(failure);
        case.held(reason, 0);
    }
}

#[test]
fn amended_coordinator_fresh_identity_pr_claim_and_local_drift_after_audit_prevent_dispatch() {
    for drift in ["identity", "pr", "pr_error", "claim", "worktree", "process"] {
        let mut case = Case::new();
        let reason = match drift {
            "identity" => {
                case.identity_drift = true;
                Refusal::ActorMismatch
            }
            "pr" => {
                case.lane.github.prs.present_on = Some(2);
                Refusal::PrPresent
            }
            "pr_error" => {
                case.lane.github.prs.fail_on = Some(2);
                Refusal::PrUnavailable
            }
            "claim" => {
                case.lane.github.change_on = Some(2);
                Refusal::ClaimChanged
            }
            "worktree" => {
                case.lane.local.changed_on_recheck = true;
                Refusal::WorktreeChanged
            }
            "process" => {
                case.lane.processes.changed_on_recheck = true;
                Refusal::ProcessConflict
            }
            _ => unreachable!(),
        };
        case.held(reason, 1);
        case.held(Refusal::Ineligible, 1);
    }
}

#[test]
fn amended_coordinator_rereads_exact_context_after_last_mutable_inspection() {
    let mut case = Case::new();
    case.lane.local.sql_on_recheck = Some(
        "UPDATE intents SET detail=json_set(detail,'$.audit_payload','changed') WHERE kind='initial_branch_removal_seal';",
    );
    case.held(Refusal::AuthorizationFailed, 1);
    let mut case = Case::new();
    case.lane.local.sql_on_recheck = Some(
        "INSERT INTO evidence(task_id,kind,payload) VALUES('task-a','held_reason','changed');",
    );
    case.held(Refusal::AuthorizationFailed, 1);
}

#[test]
fn amended_coordinator_audit_failure_rolls_back_without_dispatch() {
    let mut case = Case::new();
    amendment::database(&case.lane).execute_batch("CREATE TRIGGER audit_failure BEFORE INSERT ON intents WHEN NEW.kind='initial_branch_removal_seal' BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    case.held(Refusal::AuthorizationFailed, 0);
}

#[test]
fn amended_coordinator_invalid_saved_argv_never_reaches_inspections_or_launcher() {
    let mut case = Case::new();
    amendment::database(&case.lane)
        .execute_batch(
            "UPDATE intents SET detail=json_insert(detail,'$.args[#]','changed') WHERE kind='launch';",
        )
        .unwrap();
    case.held(Refusal::PlanInvalid, 0);
    assert_eq!(case.lane.local.inspections, 0);
}

#[test]
fn amended_coordinator_existing_audit_is_not_implicit_spawn_authorization() {
    let mut case = Case::new();
    amendment::amend(&mut case.lane).unwrap();
    case.held(Refusal::Ineligible, 1);
    assert_eq!(case.lane.local.inspections, 0);
    assert_eq!(case.lane.launcher.plans.len(), 0);
}

#[test]
fn amended_coordinator_pr_results_and_actor_refuse_before_audit() {
    for (scenario, reason) in [
        ("open", Refusal::PrPresent),
        ("stale", Refusal::PrUnavailable),
        ("pr_error", Refusal::PrUnavailable),
    ] {
        let mut case = Case::new();
        case.lane.read_scenario = scenario;
        case.lane.github.prs.present_on = Some(1);
        if scenario == "pr_error" {
            case.lane.github.prs.fail_on = Some(1);
        }
        case.held(reason, 0);
    }
    let mut case = Case::new();
    case.actor = "other";
    case.held(Refusal::ActorMismatch, 0);
}

#[test]
fn amended_coordinator_pr_absence_inspects_every_page_in_both_rounds() {
    let mut case = Case::new();
    case.lane.read_scenario = "paged_absent";
    let mut launcher = super::Launcher::default();
    assert!(matches!(
        case.run_with(&mut launcher),
        luthor::coordinator::ContinuationResult::Dispatched(_)
    ));
    assert_eq!(case.lane.github.prs.lookups, 4);
    assert_eq!(case.identity_reads, 2);
    assert_eq!(case.audit_seen, [false, true]);
    case.assert_held(1, 1);
}
