use crate::config::IssueReference;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyState {
    Open,
    ClosedIssue,
    MergedPr,
    ClosedUnmergedPr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependencyWait {
    pub dependency: IssueReference,
    pub state: DependencyState,
}
