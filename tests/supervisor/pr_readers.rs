use super::{ErrorCategory, LookupError, PullRequestReader};

pub(crate) struct ExpectedIdentityReader {
    pub(crate) login: String,
    pub(crate) fail_target: bool,
    pub(crate) target_id: u64,
    pub(crate) head_id: u64,
}
impl PullRequestReader for ExpectedIdentityReader {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok(self.login.clone())
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<serde_json::Value>, LookupError> {
        Ok(vec![])
    }
    fn detail(&mut self, _: &str, _: u64) -> Result<serde_json::Value, LookupError> {
        unreachable!()
    }
    fn repository_identity(&mut self, name: &str) -> Result<u64, LookupError> {
        if name == "org/code" && self.fail_target {
            return Err(LookupError {
                category: ErrorCategory::Transport,
                code: "offline",
                status: None,
            });
        }
        match name {
            "org/code" => Ok(self.target_id),
            "org/head" => Ok(self.head_id),
            _ => unreachable!(),
        }
    }
}

#[cfg(unix)]
#[derive(Default)]
pub(crate) struct ExitPr {
    pub(crate) ambiguous: bool,
    pub(crate) login: Option<String>,
    pub(crate) reads: usize,
    pub(crate) fail: bool,
    pub(crate) matching: Option<(String, String)>,
    pub(crate) author: Option<String>,
}
#[cfg(unix)]
impl PullRequestReader for ExitPr {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok(self.login.clone().unwrap_or_else(|| "operator".into()))
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<serde_json::Value>, LookupError> {
        self.reads += 1;
        if self.fail {
            Err(LookupError {
                category: ErrorCategory::Transport,
                code: "offline",
                status: None,
            })
        } else if let Some((issue_url, _)) = &self.matching {
            let mut prs = vec![
                serde_json::json!({"number": 42, "body": format!("Tracker-Issue: {issue_url}")}),
            ];
            if self.ambiguous {
                prs.push(serde_json::json!({"number": 43, "body": format!("Tracker-Issue: {issue_url}")}));
            }
            Ok(prs)
        } else {
            Ok(vec![])
        }
    }
    fn detail(&mut self, _: &str, number: u64) -> Result<serde_json::Value, LookupError> {
        let Some((issue_url, branch)) = &self.matching else {
            unreachable!()
        };
        Ok(serde_json::json!({
            "id": 4200 + number, "number": number, "state": "open",
            "html_url": format!("https://github.com/org/code/pull/{number}"),
            "body": format!("Tracker-Issue: {issue_url}"), "draft": true,
            "created_at": "2026-01-01T00:00:00Z",
            "base": {"repo": {"id": 10, "full_name": "org/code"}, "ref": "main"},
            "head": {"repo": {"id": 10, "full_name": "org/code"}, "ref": branch, "sha": "abc123"},
            "user": {"login": self.author.as_deref().unwrap_or("operator")}
        }))
    }
    fn repository_identity(&mut self, name: &str) -> Result<u64, LookupError> {
        match name {
            "org/code" => Ok(10),
            "org/head" => Ok(20),
            _ => Err(LookupError {
                category: ErrorCategory::Malformed,
                code: "unexpected-repository",
                status: None,
            }),
        }
    }
}
