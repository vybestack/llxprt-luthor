//! Strict in-memory GitHub writes, used with real repair receipts and Git history.
use super::*;
use luthor::{
    config::{MergeMethod, RequiredChecks},
    github::pull_request::PrMerger,
    pr_evidence::VerifiedOpenPr,
    pr_merge::{CheckSummary, MergeChecks, MergeEvidence, TrackerIssue, TrackerTarget},
};

impl PrMerger for RepairPr {
    fn merge_checks(
        &mut self,
        repo: &str,
        number: u64,
        checks: &RequiredChecks,
    ) -> Result<MergeChecks, LookupError> {
        Ok(MergeChecks {
            health: self.health(repo, number, checks)?,
            summary: vec![CheckSummary {
                name: "CI".into(),
                required: true,
                state: if self.pending { "PENDING" } else { "SUCCESS" }.into(),
            }],
        })
    }

    fn update_branch(&mut self, pr: &VerifiedOpenPr) -> Result<(), LookupError> {
        assert_eq!(head(pr), self.head);
        assert!(self.updates.is_empty(), "must not repeat an update");
        self.updates.push(self.head.clone());
        self.head = self.update_to.take().expect("expected head-pinned update");
        self.merge_state = Some("clean");
        Ok(())
    }

    fn tracker_issue(&mut self, target: &TrackerTarget) -> Result<TrackerIssue, LookupError> {
        Ok(TrackerIssue {
            target: target.clone(),
            closed_at: self.tracker_closed.then(|| "2026-10-16T12:00:00Z".into()),
        })
    }

    fn close_tracker(&mut self, _: &MergeEvidence, _: &TrackerTarget) -> Result<(), LookupError> {
        assert_eq!(self.merges.len(), 1);
        assert!(!self.tracker_closed);
        self.tracker_closed = true;
        Ok(())
    }

    fn close_finished_tracker(
        &mut self,
        _: &VerifiedOpenPr,
        _: &serde_json::Value,
        _: &TrackerTarget,
    ) -> Result<(), LookupError> {
        panic!("the exact authorized head must be merged by Luthor")
    }

    fn ready_for_review(&mut self, _: &VerifiedOpenPr) -> Result<(), LookupError> {
        panic!("fixture PR is not a draft")
    }

    fn merge_head(
        &mut self,
        pr: &VerifiedOpenPr,
        method: MergeMethod,
    ) -> Result<String, LookupError> {
        assert_eq!(method, MergeMethod::Squash);
        assert_eq!(head(pr), self.head);
        assert!(!self.pending);
        assert!(!self.conflict);
        assert!(self.failures.is_empty());
        assert!(self.merges.is_empty());
        self.merges.push(head(pr));
        Ok("c".repeat(40))
    }
}

fn head(pr: &VerifiedOpenPr) -> String {
    serde_json::to_value(pr).unwrap()["head_commit_sha"]
        .as_str()
        .unwrap()
        .into()
}
