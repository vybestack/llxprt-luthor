use crate::{
    config::Config,
    state::NeverDispatchedContext,
    supervisor::{LaunchPlan, SessionEnvironment},
};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationRefusal {
    Ineligible,
    ConfigChanged,
    EnvironmentChanged,
    EnvironmentUnavailable,
    PlanInvalid,
    ExecutableUnavailable,
    WorktreeChanged,
    StorageUnavailable,
    ArtifactConflict,
    ProcessConflict,
    ProcessUnavailable,
    ClaimChanged,
    SourceUnavailable,
    ActorMismatch,
    IdentityUnavailable,
    PrPresent,
    PrUnavailable,
    AuthorizationFailed,
    LaunchFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationResult {
    Dispatched(Box<LaunchPlan>),
    Held(ContinuationRefusal),
}

/// Read-only local checks. Implementations must not repair Git, storage, or argv.
pub trait ContinuationLocalInspector {
    fn session_environment(&mut self) -> Result<SessionEnvironment, ContinuationRefusal>;
    fn inspect(&mut self, context: &NeverDispatchedContext) -> Result<(), ContinuationRefusal>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessInspectionError {
    Conflict,
    Unavailable,
}

/// Present-day OS conflict inspection, not a receipt or historical absence proof.
/// An unassessable relevant process must return Unavailable, never success.
pub trait ContinuationProcessInspector {
    fn inspect(&mut self, context: &NeverDispatchedContext) -> Result<(), ProcessInspectionError>;
}

pub struct ContinuationDependencies<'a, P, Q, L, I, O> {
    pub task_id: &'a str,
    pub attempt_id: &'a str,
    pub config: &'a Config,
    pub config_revision: &'a str,
    pub actor: &'a str,
    pub projects: &'a mut P,
    pub prs: &'a mut Q,
    pub launcher: &'a mut L,
    pub local: &'a mut I,
    pub processes: &'a mut O,
}
