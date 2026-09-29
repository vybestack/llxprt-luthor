use luthor::github::pull_request::{
    ErrorCategory, LookupError, LookupResult, PullRequestReader, lookup,
};
use serde_json::{Value, json};
use std::collections::HashMap;

const REPO: &str = "code/project";
const ISSUE: &str = "https://github.com/tracker/issues/7";

#[derive(Default)]
struct Fake {
    pages: HashMap<u32, Vec<Value>>,
    details: HashMap<u64, Value>,
    failed_page: Option<u32>,
}

impl PullRequestReader for Fake {
    fn page(&mut self, _: &str, page: u32) -> Result<Vec<Value>, LookupError> {
        if self.failed_page == Some(page) {
            return Err(LookupError {
                category: ErrorCategory::Permission,
                code: "command-failed",
                status: Some(403),
            });
        }
        Ok(self.pages.get(&page).cloned().unwrap_or_default())
    }
    fn detail(&mut self, _: &str, number: u64) -> Result<Value, LookupError> {
        self.details.get(&number).cloned().ok_or(LookupError {
            category: ErrorCategory::Malformed,
            code: "invalid-json",
            status: None,
        })
    }
}

fn item(number: u64, body: &str) -> Value {
    json!({"number":number,"body":body})
}
fn detail(number: u64, body: &str) -> Value {
    json!({"id":number+100,"number":number,"html_url":format!("https://github.com/{REPO}/pull/{number}"),"body":body,"state":"open","draft":true,"base":{"ref":"main","repo":{"full_name":REPO}},"head":{"ref":format!("agent/{number}"),"repo":{"full_name":"fork/project"}},"user":{"login":"agent"},"mergeable_state":"dirty"})
}
fn matching(number: u64) -> (Value, Value) {
    let body = format!("body\nTracker-Issue: {ISSUE}\n");
    (item(number, &body), detail(number, &body))
}

#[test]
fn exhausts_two_pages_before_returning_absent() {
    let mut fake = Fake::default();
    fake.pages
        .insert(1, (0..100).map(|n| item(n, "other")).collect());
    fake.pages.insert(2, vec![]);
    assert_eq!(lookup(&mut fake, REPO, ISSUE), Ok(LookupResult::Absent));
}

#[test]
fn returns_linked_open_pr_even_when_draft_or_checks_fail() {
    let mut fake = Fake::default();
    let (item, details) = matching(3);
    fake.pages.insert(1, vec![item]);
    fake.details.insert(3, details);
    let LookupResult::OpenPreexisting(evidence) = lookup(&mut fake, REPO, ISSUE).unwrap() else {
        panic!("expected preexisting PR")
    };
    assert!(evidence.draft);
    assert_eq!(evidence.head_branch, "agent/3");
    assert_eq!(evidence.author, "agent");
}

#[test]
fn reports_multiple_exact_links_as_ambiguous() {
    let mut fake = Fake::default();
    let (a, ad) = matching(1);
    let (b, bd) = matching(2);
    fake.pages.insert(1, vec![a, b]);
    fake.details.insert(1, ad);
    fake.details.insert(2, bd);
    assert!(
        matches!(lookup(&mut fake, REPO, ISSUE), Ok(LookupResult::Ambiguous(found)) if found.len() == 2)
    );
}

#[test]
fn requires_exact_issue_url_and_whole_line() {
    let mut fake = Fake::default();
    fake.pages.insert(
        1,
        vec![
            item(1, "Tracker-Issue: https://github.com/tracker/issues/70"),
            item(2, format!("prefix Tracker-Issue: {ISSUE}").as_str()),
        ],
    );
    assert_eq!(lookup(&mut fake, REPO, ISSUE), Ok(LookupResult::Absent));
}

#[test]
fn malformed_details_and_page_failures_never_become_absent() {
    let mut fake = Fake::default();
    fake.pages
        .insert(1, vec![item(1, &format!("Tracker-Issue: {ISSUE}"))]);
    fake.details.insert(1, json!({"id":101,"state":"closed"}));
    assert_eq!(
        lookup(&mut fake, REPO, ISSUE).unwrap_err().category,
        ErrorCategory::Malformed
    );
    let mut failed = Fake {
        failed_page: Some(1),
        ..Fake::default()
    };
    assert_eq!(
        lookup(&mut failed, REPO, ISSUE).unwrap_err().category,
        ErrorCategory::Permission
    );
}

#[test]
fn failed_later_page_is_not_reported_as_absent() {
    let mut fake = Fake {
        failed_page: Some(2),
        ..Fake::default()
    };
    fake.pages
        .insert(1, (0..100).map(|n| item(n, "unrelated")).collect());
    assert!(lookup(&mut fake, REPO, ISSUE).is_err());
}
