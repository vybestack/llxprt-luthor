use luthor::{
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    eligibility::Candidate,
};

pub(super) fn candidate(issue_id: &str, number: u64) -> Candidate {
    Candidate {
        project_id: "project-1".into(),
        item_id: format!("item-{issue_id}"),
        repository: "org/tracker".into(),
        issue_node_id: issue_id.into(),
        issue_number: number,
        issue_url: format!("https://github.com/org/tracker/issues/{number}"),
        tracker_repo_id: "repo-node-id".into(),
        milestone_id: None,
        milestone_title: None,
        observed_at_unix_secs: 123,
        observed_state: "open".into(),
        observed_assignees: vec![],
        observed_labels: vec!["ready".into()],
        observed_project_fields: vec![],
        marker: Marker::Label {
            name: "ready".into(),
        },
        mapping: Mapping {
            tracker_repository: "org/tracker".into(),
            code_repository: "org/code".into(),
            checkout: "/checkout".into(),
            base_branch: "main".into(),
            push_remote: "origin".into(),
            allowed_pr_head_repository: "bot/fork".into(),
            allowed_pr_author: "bot".into(),
        },
        source: Source {
            project_id: "project-1".into(),
            repositories: vec!["org/tracker".into()],
            ready_marker: Marker::Label {
                name: "ready".into(),
            },
            milestone: None,
        },
    }
}

pub(super) fn config() -> Config {
    Config {
        state_root: "/state".into(),
        worktree_root: "/worktrees".into(),
        capacity: 1,
        assignment_login: "bot".into(),
        sources: vec![Source {
            project_id: "project-1".into(),
            repositories: vec!["org/tracker".into()],
            ready_marker: Marker::Label {
                name: "ready".into(),
            },
            milestone: None,
        }],
        mappings: vec![Mapping {
            tracker_repository: "org/tracker".into(),
            code_repository: "org/code".into(),
            checkout: "/checkout".into(),
            base_branch: "main".into(),
            push_remote: "origin".into(),
            allowed_pr_head_repository: "bot/fork".into(),
            allowed_pr_author: "bot".into(),
        }],
        initial: CommandTemplate {
            executable: "/bin/worker".into(),
            args: vec!["start".into(), "{task.id}".into()],
        },
        resume: CommandTemplate {
            executable: "/bin/worker".into(),
            args: vec!["resume".into(), "{task.id}".into()],
        },
    }
}
