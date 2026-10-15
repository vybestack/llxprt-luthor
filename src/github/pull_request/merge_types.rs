use super::{LookupError, PrHealthReader};
use crate::pr_merge::MergeChecks;
use crate::{config::MergeMethod, model::VerifiedOpenPr};

/// Writes are deliberately not part of the read/repair port.
pub trait PrMerger: PrHealthReader {
    fn merge_checks(
        &mut self,
        repository: &str,
        number: u64,
        checks: &crate::config::RequiredChecks,
    ) -> Result<MergeChecks, LookupError>;
    fn update_branch(&mut self, pr: &VerifiedOpenPr) -> Result<(), LookupError>;
    fn tracker_issue(
        &mut self,
        target: &crate::pr_merge::TrackerTarget,
    ) -> Result<crate::pr_merge::TrackerIssue, LookupError>;
    fn close_tracker(
        &mut self,
        merge: &crate::pr_merge::MergeEvidence,
        target: &crate::pr_merge::TrackerTarget,
    ) -> Result<(), LookupError>;
    fn close_finished_tracker(
        &mut self,
        pr: &VerifiedOpenPr,
        terminal: &serde_json::Value,
        target: &crate::pr_merge::TrackerTarget,
    ) -> Result<(), LookupError>;
    fn ready_for_review(&mut self, pr: &VerifiedOpenPr) -> Result<(), LookupError>;
    fn merge_head(
        &mut self,
        pr: &VerifiedOpenPr,
        method: MergeMethod,
    ) -> Result<String, LookupError>;
}
