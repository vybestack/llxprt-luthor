//! Bounded repair diagnostics, selected at the failed check, never from error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairHoldCause {
    PrIdentityUnverified,
    OwnerLockBusy,
    OwnerUnavailable,
    OwnerProtocolUnverified,
    AttemptEvidenceUnverified,
    ProcessEvidenceUnverified,
    ReceiptUnverified,
    AttemptSettling,
    SessionUnverified,
}

impl RepairHoldCause {
    pub fn detail(self) -> &'static str {
        match self {
            Self::PrIdentityUnverified => "pr-identity-unverified",
            Self::OwnerLockBusy => {
                "owner-lock-busy; restore will retry next cycle; wait for the owner to release the lock"
            }
            Self::OwnerUnavailable => "owner-unavailable",
            Self::OwnerProtocolUnverified => "owner-protocol-unverified",
            Self::AttemptEvidenceUnverified => "attempt-evidence-unverified",
            Self::ProcessEvidenceUnverified => "process-evidence-unverified",
            Self::ReceiptUnverified => "receipt-unverified",
            Self::AttemptSettling => {
                "attempt-settling; restore will retry next cycle; wait for process and receipt accounting"
            }
            Self::SessionUnverified => "session-unverified",
        }
    }

    pub fn reason(self) -> String {
        format!("PR repair held: {}", self.detail())
    }
}

/// Exact allowlist, not a prefix match. Restoration still requires all existing
/// PR, claim, worktree, receipt and quiescence proofs and an unamended session.
pub(crate) fn open_hold_reasons() -> Vec<String> {
    let mut reasons: Vec<_> = [
        RepairHoldCause::PrIdentityUnverified,
        RepairHoldCause::OwnerLockBusy,
        RepairHoldCause::OwnerUnavailable,
        RepairHoldCause::OwnerProtocolUnverified,
        RepairHoldCause::AttemptEvidenceUnverified,
        RepairHoldCause::ProcessEvidenceUnverified,
        RepairHoldCause::ReceiptUnverified,
        RepairHoldCause::AttemptSettling,
    ]
    .into_iter()
    .map(RepairHoldCause::reason)
    .collect();
    reasons.extend([
        "exit PR present".into(),
        "PR repair held: PR, process, receipt or ownership evidence did not verify".into(),
        "PR repair held: worktree identity or tip did not verify".into(),
    ]);
    reasons
}

impl std::fmt::Display for RepairHoldCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.detail())
    }
}
