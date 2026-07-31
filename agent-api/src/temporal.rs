//! Temporal gateway state types.
//!
//! These types make recent gateway history observable without requiring durable
//! storage. Implementations should keep retention bounded.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::ontology::{EntityRef, EvidenceValue};
use crate::types::GatewaySnapshot;

/// Default recent-history retention for in-memory implementations.
pub const DEFAULT_TEMPORAL_RETENTION: usize = 64;

/// Query over recent temporal state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct TemporalQuery {
    /// Include events observed since this many seconds ago.
    pub since_seconds: Option<u64>,
    /// Include events with sequence greater than or equal to this value.
    pub from_sequence: Option<u64>,
    /// Include events with sequence less than or equal to this value.
    pub to_sequence: Option<u64>,
}

/// A semantic event in recent gateway history.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TemporalEvent {
    pub sequence: u64,
    pub timestamp_unix_ms: u64,
    pub kind: TemporalEventKind,
    pub subject: EntityRef,
    pub summary: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, EvidenceValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TemporalEventKind {
    RouteChanged,
    ListenerChanged,
    EndpointChanged,
    BackendChanged,
    CircuitBreakerChanged,
    RateLimiterChanged,
    AdminAction,
    SnapshotRecorded,
}

/// Public bounded timeline response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TimelineSnapshot {
    pub retention: usize,
    pub generated_at_unix_ms: u64,
    pub events: Vec<TemporalEvent>,
    pub snapshots: Vec<SnapshotHistoryEntry>,
}

/// Compact public gateway snapshot history entry.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotHistoryEntry {
    pub sequence: u64,
    pub timestamp_unix_ms: u64,
    pub snapshot: GatewaySnapshot,
}

/// Semantic diff over a recent time window.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GatewayDiff {
    pub from_sequence: Option<u64>,
    pub to_sequence: u64,
    pub since_seconds: Option<u64>,
    pub route_count_delta: isize,
    pub open_circuits_delta: isize,
    pub exhausted_rate_limiters_delta: isize,
    pub added_routes: Vec<String>,
    pub removed_routes: Vec<String>,
    pub changed_routes: Vec<String>,
    pub added_listeners: Vec<String>,
    pub removed_listeners: Vec<String>,
    pub changed_circuit_breakers: Vec<String>,
    pub changed_rate_limiters: Vec<String>,
    pub events: Vec<TemporalEvent>,
}
