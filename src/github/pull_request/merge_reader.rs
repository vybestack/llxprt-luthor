//! Single-shot writes: a failed/uncertain mutation is never blindly retried.
use super::merge_validation::valid_sha;
use super::{
    GhPullRequestReader, LookupError, MergeChecks, PrMerger,
    merge_write::{authorize, write},
    types::{ErrorCategory, error},
};
use crate::{config::MergeMethod, model::VerifiedOpenPr};
use serde_json::Value;

impl PrMerger for GhPullRequestReader {
    fn merge_checks(
        &mut self,
        repository: &str,
        number: u64,
        checks: &crate::config::RequiredChecks,
    ) -> Result<MergeChecks, LookupError> {
        super::health_reader::observe_checks(repository, number, checks, true, |args| {
            self.api(args)
        })
    }

    fn update_branch(&mut self, pr: &VerifiedOpenPr) -> Result<(), LookupError> {
        super::merge_followup::update(self, pr)
    }

    fn tracker_issue(
        &mut self,
        target: &crate::pr_merge::TrackerTarget,
    ) -> Result<crate::pr_merge::TrackerIssue, LookupError> {
        super::merge_followup::issue(self, target)
    }

    fn close_tracker(
        &mut self,
        merge: &crate::pr_merge::MergeEvidence,
        target: &crate::pr_merge::TrackerTarget,
    ) -> Result<(), LookupError> {
        super::merge_followup::close(self, merge, target)
    }

    fn ready_for_review(&mut self, pr: &VerifiedOpenPr) -> Result<(), LookupError> {
        authorize(self, pr)?;
        write(
            self,
            &[
                "pr",
                "ready",
                &pr.number.to_string(),
                "--repo",
                &pr.repository,
            ],
        )?;
        Ok(())
    }

    fn merge_head(
        &mut self,
        pr: &VerifiedOpenPr,
        method: MergeMethod,
    ) -> Result<String, LookupError> {
        authorize(self, pr)?;
        let detail = self.api(&[
            "api",
            &format!("repos/{}/pulls/{}", pr.repository, pr.number),
        ])?;
        super::merge_validation::validate_identity(&detail, pr)?;
        if detail["draft"] != false
            || detail["mergeable"] != true
            || detail["mergeable_state"] != "clean"
        {
            return Err(error(ErrorCategory::Unknown, "merge-no-longer-clean", None));
        }
        let endpoint = format!("repos/{}/pulls/{}/merge", pr.repository, pr.number);
        let sha = format!("sha={}", pr.head_commit_sha);
        let method = format!("merge_method={}", method.as_str());
        let output = write(
            self,
            &[
                "api", "--method", "PUT", &endpoint, "-f", &sha, "-f", &method,
            ],
        )?;
        let result: Value = serde_json::from_slice(&output).map_err(|_| malformed())?;
        if result["merged"] != true {
            return Err(error(ErrorCategory::Unknown, "merge-refused", None));
        }
        result["sha"]
            .as_str()
            .filter(|sha| valid_sha(sha))
            .map(str::to_owned)
            .ok_or_else(malformed)
    }
}

fn malformed() -> LookupError {
    error(ErrorCategory::Malformed, "invalid-merge-response", None)
}
