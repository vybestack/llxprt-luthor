use crate::{
    config::Config,
    model::{
        EffectiveConfigSnapshot, ExitPrEvidence, LaunchPlan, SelectionEvidence, WorktreeIdentity,
        WorktreeIntent,
    },
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Saved assignment intent, not a fresh observation of GitHub ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedClaimAssignment {
    pub principal: String,
    pub repository: String,
    pub number: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NeverDispatchedReason {
    LegacyPreflightRecovery,
}

/// SQLite eligibility only. This cannot establish OS quiescence or authorize a spawn.
/// Private fields bind authorization to the exact rows read from this store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeverDispatchedContext {
    pub(crate) root: PathBuf,
    pub(crate) snapshot: SavedRows,
    pub(crate) amendment: Option<(i64, InitialBranchRemovalAudit)>,
    pub(crate) saved_launch_plan: String,
    pub(crate) plan: LaunchPlan,
    pub(crate) selection: SelectionEvidence,
    pub(crate) claim: SavedClaimAssignment,
    pub(crate) claim_verified: String,
    pub(crate) worktree_intent: WorktreeIntent,
    pub(crate) worktree_identity: WorktreeIdentity,
}

impl NeverDispatchedContext {
    pub fn task_id(&self) -> &str {
        &self.plan.task_id
    }
    pub fn attempt_id(&self) -> &str {
        &self.plan.attempt_id
    }
    pub fn saved_launch_plan(&self) -> &str {
        &self.saved_launch_plan
    }
    pub fn plan(&self) -> &LaunchPlan {
        &self.plan
    }
    pub fn selection(&self) -> &SelectionEvidence {
        &self.selection
    }
    pub fn claim(&self) -> &SavedClaimAssignment {
        &self.claim
    }
    pub fn claim_verified(&self) -> &str {
        &self.claim_verified
    }
    pub fn worktree_intent(&self) -> &WorktreeIntent {
        &self.worktree_intent
    }
    pub fn worktree_identity(&self) -> &WorktreeIdentity {
        &self.worktree_identity
    }
}

/// Receipt for the committed audit only, not proof of any process activity.
#[derive(Debug)]
pub struct NeverDispatchedAuthorization {
    pub(crate) context: NeverDispatchedContext,
    pub(crate) audit_sequence: i64,
}

impl NeverDispatchedAuthorization {
    pub fn context(&self) -> &NeverDispatchedContext {
        &self.context
    }
    pub fn audit_sequence(&self) -> i64 {
        self.audit_sequence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedRows(pub(crate) Vec<Vec<String>>);

pub(crate) const KIND: &str = "initial_branch_removed";
pub(crate) const SEAL_KIND: &str = "initial_branch_removal_seal";
pub(crate) const MAX_AUDIT_BYTES: usize = 262_144;
pub(crate) const MAX_SEAL_BYTES: usize = MAX_AUDIT_BYTES * 2 + 128;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AmendmentSeal {
    pub(crate) audit_sequence: i64,
    pub(crate) audit_payload: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchRemovalReason {
    NativeInitialBranchIsConversation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProcessQuiescence {
    Clear { observed_at_unix_secs: u64 },
    Conflict { observed_at_unix_secs: u64 },
    Unavailable,
}

/// Inputs from fresh external inspections, not assertions inferred from SQLite.
/// The coordinator must inspect OS artifacts, worktree, claim and PR before calling.
pub struct BranchRemovalRequest<'a> {
    pub actor: &'a str,
    pub config: &'a Config,
    pub config_revision: &'a str,
    pub pr: &'a ExitPrEvidence,
    pub processes: ProcessQuiescence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchRemovalDelta {
    pub index: usize,
    pub removed: [String; 2],
}

/// Config provenance for future tasks only, never authority to resume this task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case", deny_unknown_fields)]
pub enum FutureTemplateCorrection {
    NativeInitialAndResumeBranchRemovalV1 {
        initial: BranchRemovalDelta,
        resume: BranchRemovalDelta,
    },
}

/// One bounded append-only evidence row. Original launch intent remains untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitialBranchRemovalAudit {
    pub schema_version: u32,
    pub prompt_version: crate::model::SavedPromptVersion,
    pub actor: String,
    pub reason_code: BranchRemovalReason,
    pub authorized_at_unix_secs: u64,
    pub task_id: String,
    pub attempt_id: String,
    pub saved_launch_plan: String,
    pub original_plan: LaunchPlan,
    pub effective_plan: LaunchPlan,
    pub delta: BranchRemovalDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub future_template_correction: Option<FutureTemplateCorrection>,
    pub current_config_revision: String,
    pub current_config: EffectiveConfigSnapshot,
    pub pr: ExitPrEvidence,
    pub processes: ProcessQuiescence,
    pub(crate) original_rows: SavedRows,
}

impl NeverDispatchedContext {
    /// Saved plan stays the original contract; callers must explicitly opt in here.
    pub fn effective_plan(&self) -> &LaunchPlan {
        self.amendment()
            .map_or(self.plan(), |audit| &audit.effective_plan)
    }

    pub fn amendment(&self) -> Option<&InitialBranchRemovalAudit> {
        self.amendment.as_ref().map(|(_, audit)| audit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmendedDispatchProof {
    pub amendment_sequence: i64,
    pub effective_plan: LaunchPlan,
}

pub(crate) fn valid_actor(actor: &str, context: &NeverDispatchedContext) -> bool {
    !actor.is_empty()
        && actor.len() <= 128
        && actor
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && actor == context.selection.candidate.mapping.allowed_pr_author
}
