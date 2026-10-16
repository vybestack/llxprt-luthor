//! PR identity observations shared by repair and exact-head merge fixtures.
use super::*;

impl PullRequestReader for RepairPr {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        self.inner.authenticated_identity()
    }
    fn page(&mut self, repo: &str, page: u32) -> Result<Vec<serde_json::Value>, LookupError> {
        self.inner.page(repo, page)
    }
    fn detail(&mut self, repo: &str, number: u64) -> Result<serde_json::Value, LookupError> {
        let mut detail = self.inner.detail(repo, number)?;
        detail["head"]["sha"] = serde_json::json!(self.head);
        if self.wrong_pr_id {
            detail["id"] = serde_json::json!(9999);
        }
        if let Some(state) = self.merge_state {
            detail["mergeable"] = serde_json::json!(true);
            detail["mergeable_state"] = serde_json::json!(state);
            detail["draft"] = serde_json::json!(false);
            detail["merged"] = serde_json::json!(false);
        }
        if !self.merges.is_empty() {
            detail["state"] = serde_json::json!("closed");
            detail["merged"] = serde_json::json!(true);
            detail["merged_at"] = serde_json::json!("2026-10-16T12:00:00Z");
            detail["merge_commit_sha"] = serde_json::json!("c".repeat(40));
        }
        Ok(detail)
    }
    fn repository_identity(&mut self, repo: &str) -> Result<u64, LookupError> {
        self.inner.repository_identity(repo)
    }
}
