//! Safe operator action types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ontology::{ActionKind, EntityRef};
use crate::temporal::TemporalEvent;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActionResult {
    pub action_id: String,
    pub kind: ActionKind,
    pub target: EntityRef,
    pub status: ActionStatus,
    pub risk: ActionRisk,
    pub preconditions: Vec<ActionPrecondition>,
    pub before: ActionEvidence,
    pub after: ActionEvidence,
    pub rollback: Option<RollbackMetadata>,
    pub expires_at_unix_ms: Option<u64>,
    pub timeline_event: TemporalEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Applied,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionRisk {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActionPrecondition {
    pub name: String,
    pub passed: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActionEvidence {
    pub backend: String,
    pub was_draining: bool,
    pub is_draining: bool,
    pub affected_routes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RollbackMetadata {
    pub action: ActionKind,
    pub cli_command: String,
    pub reason: String,
}
