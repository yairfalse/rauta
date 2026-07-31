//! Agent API Types
//!
//! All types used by the agent query interface. These derive `Serialize`, `Deserialize`,
//! and `JsonSchema` for MCP tool integration and CLI output.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ontology::OntologyEvidence;

/// Gateway status overview
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GatewaySnapshot {
    pub status: String,
    pub uptime_seconds: u64,
    pub route_count: usize,
    pub open_circuits: usize,
    pub exhausted_rate_limiters: usize,
    pub listeners: Vec<ListenerSnapshot>,
    pub cache_stats: Option<CacheStats>,
}

/// Single route with backends and filters
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RouteSnapshot {
    pub pattern: String,
    pub method: String,
    pub backends: Vec<BackendSnapshot>,
    pub has_request_filters: bool,
    pub has_response_filters: bool,
    pub has_redirect: bool,
    pub has_timeout: bool,
    pub has_retry: bool,
}

/// Backend server status
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BackendSnapshot {
    pub address: String,
    pub port: u16,
    pub weight: u16,
    pub is_ipv6: bool,
    pub is_draining: bool,
    pub health_score: Option<f64>,
}

/// Circuit breaker state
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CircuitBreakerSnapshot {
    pub backend_id: String,
    pub state: String,
    pub failure_count: u32,
}

/// Rate limiter bucket state
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RateLimiterSnapshot {
    pub route: String,
    pub tokens_available: f64,
    pub capacity: f64,
    pub refill_rate: f64,
}

/// Active listener
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListenerSnapshot {
    pub port: u16,
    pub protocol: String,
    pub gateway_refs: Vec<String>,
}

/// Route cache statistics
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub size: usize,
    pub hit_rate: f64,
}

/// Diagnostic rule severity
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// Diagnostic result from the diagnostics engine
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Diagnosis {
    pub rule_id: String,
    pub symptom: String,
    pub severity: Severity,
    pub confidence: f64,
    pub causal_chain: Vec<String>,
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ontology_evidence: Vec<OntologyEvidence>,
    pub suggested_actions: Vec<SuggestedAction>,
}

/// Suggested action from a diagnosis
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SuggestedAction {
    pub description: String,
    pub cli_command: Option<String>,
}

/// Metrics snapshot (Prometheus metrics as structured data)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MetricSnapshot {
    pub name: String,
    pub help: String,
    pub metric_type: String,
    pub values: Vec<MetricValue>,
}

/// Single metric value with labels
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MetricValue {
    pub labels: std::collections::HashMap<String, String>,
    pub value: f64,
}
