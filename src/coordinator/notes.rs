use serde::Serialize;

/// A task together with the specific reason it was skipped, held or retained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskNote {
    pub task_id: String,
    pub reason: String,
}
