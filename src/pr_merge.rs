//! Durable authorization and evidence for opt-in exact-head merges.
use crate::{config::MergePolicy, github::pull_request::PrHealth, model::VerifiedOpenPr};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeAuthorization {
    pub pr: VerifiedOpenPr,
    pub policy: MergePolicy,
    pub checks: MergeChecks,
    pub observed_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeEvidence {
    pub authorization: MergeAuthorization,
    pub merge_commit: String,
    pub merged_head: String,
    pub observed_at: u64,
}

/// Tracker identity comes from the saved selection, never the PR's closing text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerTarget {
    pub repository: String,
    pub number: u64,
    pub node_id: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerIssue {
    pub target: TrackerTarget,
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerClosureEvidence {
    pub issue: TrackerIssue,
    pub merge: MergeEvidence,
    pub closed_by_luthor: bool,
    pub observed_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchUpdate {
    pub pr: VerifiedOpenPr,
    pub observed_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeChecks {
    pub health: PrHealth,
    pub summary: Vec<CheckSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSummary {
    pub name: String,
    pub required: bool,
    pub state: String,
}
