pub(crate) fn observation_stage(kind: &str) -> &str {
    match kind {
        "pause_pr_lookup" => "pause_pr_lookup",
        "exit_pr_lookup" => "exit_pr_lookup",
        "retry_pr_lookup" => "retry_pr_lookup",
        "source_observation" => "source_observation",
        _ => unreachable!(),
    }
}
pub(crate) fn pr_stage_label(stage: Option<&str>) -> &str {
    match stage {
        Some("pause_pr_lookup") => "pause",
        Some("retry_pr_lookup") => "retry",
        _ => "exit",
    }
}
