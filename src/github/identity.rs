use crate::eligibility::Candidate;
use std::{path::Path, process::Command};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("GitHub account verification failed")]
    Command,
    #[error("GitHub account verification returned missing or ambiguous account output")]
    Output,
    #[error("GitHub account is not the authorized PR author for live writes")]
    Mismatch,
}

pub fn verify_authenticated_account(
    gh_binary: &Path,
    candidate: &Candidate,
) -> Result<(), IdentityError> {
    let output = Command::new(gh_binary)
        .args(["api", "user", "--jq", ".login"])
        .output()
        .map_err(|_| IdentityError::Command)?;
    if !output.status.success() {
        return Err(IdentityError::Command);
    }
    let stdout = std::str::from_utf8(&output.stdout).map_err(|_| IdentityError::Output)?;
    let mut lines = stdout.lines();
    let login = lines.next().ok_or(IdentityError::Output)?.trim();
    if login.is_empty() || lines.any(|line| !line.trim().is_empty()) {
        return Err(IdentityError::Output);
    }
    if login != "acoliver" || candidate.mapping.allowed_pr_author != login {
        return Err(IdentityError::Mismatch);
    }
    Ok(())
}
