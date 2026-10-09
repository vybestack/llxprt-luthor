mod amended_completion;
mod amended_completion_lookup;
pub(crate) mod amended_dispatch;
mod amended_observation;
mod amendment;
mod attempts;
mod branch_removal;
mod context;
mod continuation;
mod continuation_hold;
mod database;
mod dispatch_stages;
pub mod exit_observation;
pub(crate) mod exits;
pub mod journal;
pub mod launches;
pub mod pr_completion;
mod proofs;
pub mod scheduling;
pub mod task_records;
pub mod worktree_records;
pub(crate) use crate::model::OperatorRecoveryAudit;
pub use crate::model::{
    EffectiveConfigSnapshot, ExitPrEvidence, ExitReceipt, LaunchPlan, PausePrEvidence,
    PausePrStatus, Reconciliation, RecoveryInspection, RetryAuthorization, RetrySourceEvidence,
    SelectionEvidence, SessionEnvironment, StateError, TerminalExitBasis, TerminalExitProof,
    WorktreeIdentity, WorktreeIntent, WorktreeRecord,
};
pub use amended_dispatch::{verify_amended_observation_plan, verify_amended_worker_plan};
pub(crate) use amendment::policy::{initial_branch_removal_plan, validate_removal_config};
pub(crate) use attempts::attempt_selection;
pub(crate) use attempts::same_task_config;
pub use context::{
    AmendedDispatchProof, BranchRemovalDelta, BranchRemovalReason, BranchRemovalRequest,
    FutureTemplateCorrection, InitialBranchRemovalAudit, ProcessQuiescence,
};
pub use context::{
    NeverDispatchedAuthorization, NeverDispatchedContext, NeverDispatchedReason,
    SavedClaimAssignment,
};

pub use database::StateStore;
pub use launches::retry_context_for_task;
pub(crate) use launches::{hold_retry_intent, initial_launch, selection_for_attempt};

use crate::config::Config;

impl NeverDispatchedContext {
    /// Recheck the exact saved rows and commit the audit before any continuation
    /// side effects. External read-only validation remains the caller's duty.
    pub fn authorize(
        &self,
        store: &mut StateStore,
        actor: &str,
        reason: NeverDispatchedReason,
    ) -> Result<NeverDispatchedAuthorization, StateError> {
        continuation::authorize(&mut store.connection, &store.root, self, actor, reason)
    }
}

impl StateStore {
    pub fn authorize_initial_branch_removal(
        &mut self,
        context: &NeverDispatchedContext,
        effective: &LaunchPlan,
        request: BranchRemovalRequest<'_>,
    ) -> Result<i64, StateError> {
        branch_removal::authorize_initial_branch_removal(
            &mut self.connection,
            &self.root,
            context,
            effective,
            request,
        )
    }

    pub fn amended_dispatch_proof(
        &self,
        context: &NeverDispatchedContext,
        config: &Config,
        revision: &str,
    ) -> Result<AmendedDispatchProof, StateError> {
        amended_dispatch::amended_dispatch_proof(
            &self.connection,
            &self.root,
            context,
            config,
            revision,
        )
    }

    pub fn begin_amended_supervision(
        &mut self,
        context: &NeverDispatchedContext,
        config: &Config,
        revision: &str,
        owner_protocol: &crate::ownership::WorktreeOwnerProtocol,
    ) -> Result<AmendedDispatchProof, StateError> {
        amended_dispatch::begin_amended_supervision(
            &mut self.connection,
            &self.root,
            context,
            config,
            revision,
            owner_protocol,
        )
    }

    /// Inspect one held, first-and-latest saved launch and reservation without
    /// reading files, observing processes or GitHub, or changing state.
    pub fn never_dispatched_context(
        &self,
        task_id: &str,
        attempt_id: &str,
    ) -> Result<NeverDispatchedContext, StateError> {
        let tx = self.connection.unchecked_transaction()?;
        let context = continuation::read_context(&tx, &self.root, task_id, attempt_id)?;
        tx.commit()?;
        Ok(context)
    }
}

#[cfg(test)]
mod recovery_tests;
