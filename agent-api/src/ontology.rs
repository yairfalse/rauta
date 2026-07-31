//! RAUTA ontology contract.
//!
//! The ontology gives agents stable entities, evidence, actions, time windows,
//! and causal links instead of requiring them to parse display strings.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ONTOLOGY_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Listener,
    Route,
    RouteMatch,
    Policy,
    Backend,
    Endpoint,
    TrafficFlow,
    HealthSignal,
    Failure,
    Evidence,
    Action,
    TimeWindow,
    CausalLink,
    Gateway,
    Cache,
    RateLimiter,
    CircuitBreaker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EntityRef {
    pub kind: EntityKind,
    pub id: String,
}

impl EntityRef {
    pub fn new(kind: EntityKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum EvidenceValue {
    String(String),
    Integer(i64),
    Float(f64),
    Bool(bool),
}

impl From<&str> for EvidenceValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<String> for EvidenceValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<u64> for EvidenceValue {
    fn from(value: u64) -> Self {
        Self::Integer(value as i64)
    }
}

impl From<u32> for EvidenceValue {
    fn from(value: u32) -> Self {
        Self::Integer(value as i64)
    }
}

impl From<usize> for EvidenceValue {
    fn from(value: usize) -> Self {
        Self::Integer(value as i64)
    }
}

impl From<f64> for EvidenceValue {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<bool> for EvidenceValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    CircuitBreakerState,
    RateLimiterState,
    BackendHealth,
    BackendDrainState,
    CacheState,
    ListenerState,
    RouteState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OntologyEvidence {
    pub schema_version: u16,
    pub evidence_id: String,
    pub kind: EvidenceKind,
    pub subject: EntityRef,
    pub attributes: BTreeMap<String, EvidenceValue>,
    pub summary: String,
}

impl OntologyEvidence {
    pub fn new(
        evidence_id: impl Into<String>,
        kind: EvidenceKind,
        subject: EntityRef,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: ONTOLOGY_SCHEMA_VERSION,
            evidence_id: evidence_id.into(),
            kind,
            subject,
            attributes: BTreeMap::new(),
            summary: summary.into(),
        }
    }

    pub fn with_attr(mut self, key: impl Into<String>, value: impl Into<EvidenceValue>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimeWindow {
    pub start_unix_ms: u64,
    pub end_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Inspect,
    DrainBackend,
    UndrainBackend,
    QuarantineBackend,
    AdjustRateLimit,
    ClearCache,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActionRef {
    pub kind: ActionKind,
    pub target: EntityRef,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CausalLink {
    pub from: EntityRef,
    pub to: EntityRef,
    pub relation: String,
}
