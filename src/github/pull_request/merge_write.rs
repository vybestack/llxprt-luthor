//! Single-shot mutation transport and fresh account/PR authorization.
use super::{
    GhPullRequestReader, LookupError, PullRequestReader,
    types::{ErrorCategory, error},
};
use crate::model::VerifiedOpenPr;
use std::process::Command;

pub(crate) fn authorize(
    reader: &mut GhPullRequestReader,
    pr: &VerifiedOpenPr,
) -> Result<(), LookupError> {
    if reader.authenticated_identity()? != pr.active_login || pr.active_login != "acoliver" {
        return Err(error(
            ErrorCategory::Permission,
            "merge-identity-mismatch",
            None,
        ));
    }
    let detail = reader.api(&[
        "api",
        &format!("repos/{}/pulls/{}", pr.repository, pr.number),
    ])?;
    super::merge_validation::validate_identity(&detail, pr)?;
    if detail["state"] != "open" {
        return Err(error(ErrorCategory::Malformed, "merge-not-open", None));
    }
    Ok(())
}

pub(crate) fn write(reader: &GhPullRequestReader, args: &[&str]) -> Result<Vec<u8>, LookupError> {
    crate::github::budget::command(args);
    let output = Command::new(&reader.executable)
        .args(args)
        .arg("--include")
        .output()
        .map_err(|_| error(ErrorCategory::Transport, "merge-write-transport", None))?;
    crate::github::budget::headers(&output.stdout, args.contains(&"graphql"))
        .map_err(|_| error(ErrorCategory::Malformed, "quota-header-malformed", None))?;
    if !output.status.success() {
        return Err(error(
            ErrorCategory::Unknown,
            "merge-write-failed",
            crate::github::read::http_status(&output.stdout, &output.stderr),
        ));
    }
    Ok(crate::github::read::body(&output.stdout).to_vec())
}
