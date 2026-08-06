//! Diagnostics Engine
//!
//! Deterministic Rust rules that correlate gateway state, explain failures,
//! and suggest actions. No LLM — pure structured reasoning.

use crate::ebpf::TcpHealthEvidenceSnapshot;
use crate::types::{
    CircuitBreakerSnapshot, Diagnosis, GatewaySnapshot, RateLimiterSnapshot, RouteSnapshot,
};

/// Context available to diagnostic rules
pub struct DiagnosticContext {
    pub snapshot: GatewaySnapshot,
    pub routes: Vec<RouteSnapshot>,
    pub circuit_breakers: Vec<CircuitBreakerSnapshot>,
    pub rate_limiters: Vec<RateLimiterSnapshot>,
    pub tcp_health: Option<TcpHealthEvidenceSnapshot>,
}

/// A diagnostic rule that evaluates gateway state
pub trait DiagnosticRule: Send + Sync {
    /// Unique rule ID (e.g., "RAUTA-CB-001")
    fn id(&self) -> &str;

    /// Human-readable symptom this rule detects
    fn symptom(&self) -> &str;

    /// Evaluate the rule against current gateway state
    fn evaluate(&self, ctx: &DiagnosticContext) -> Vec<Diagnosis>;
}

/// Engine that runs diagnostic rules against gateway state
pub struct DiagnosticsEngine {
    rules: Vec<Box<dyn DiagnosticRule>>,
}

impl DiagnosticsEngine {
    /// Create a new engine with no rules
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Create engine with all built-in rules
    pub fn with_builtin_rules() -> Self {
        let mut engine = Self::new();
        engine.register(Box::new(super::rules::CircuitBreakerCascade));
        engine.register(Box::new(super::rules::CircuitBreakerOpen));
        engine.register(Box::new(super::rules::RateLimitExhausted));
        engine.register(Box::new(super::rules::NoHealthyBackends));
        engine.register(Box::new(super::rules::AllBackendsDraining));
        engine.register(Box::new(super::rules::LowCacheHitRate));
        engine.register(Box::new(super::rules::ListenerConflict));
        engine.register(Box::new(super::rules::TcpHealthAnomaly));
        engine
    }

    /// Register a custom diagnostic rule
    pub fn register(&mut self, rule: Box<dyn DiagnosticRule>) {
        self.rules.push(rule);
    }

    /// Run all rules and return diagnoses
    pub fn diagnose(&self, ctx: &DiagnosticContext) -> Vec<Diagnosis> {
        let mut diagnoses = Vec::new();
        for rule in &self.rules {
            diagnoses.extend(rule.evaluate(ctx));
        }
        diagnoses
    }

    /// Run rules matching a specific symptom keyword
    pub fn diagnose_symptom(&self, ctx: &DiagnosticContext, symptom: &str) -> Vec<Diagnosis> {
        let symptom_lower = symptom.to_lowercase();
        let mut diagnoses = Vec::new();
        for rule in &self.rules {
            if rule.symptom().to_lowercase().contains(&symptom_lower)
                || rule.id().to_lowercase().contains(&symptom_lower)
            {
                diagnoses.extend(rule.evaluate(ctx));
            }
        }
        diagnoses
    }
}

impl Default for DiagnosticsEngine {
    fn default() -> Self {
        Self::with_builtin_rules()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Severity;

    fn empty_context() -> DiagnosticContext {
        DiagnosticContext {
            snapshot: GatewaySnapshot {
                status: "ok".to_string(),
                uptime_seconds: 3600,
                route_count: 0,
                open_circuits: 0,
                exhausted_rate_limiters: 0,
                listeners: vec![],
                cache_stats: None,
            },
            routes: vec![],
            circuit_breakers: vec![],
            rate_limiters: vec![],
            tcp_health: None,
        }
    }

    #[test]
    fn test_engine_returns_empty_for_healthy_gateway() {
        let engine = DiagnosticsEngine::with_builtin_rules();
        let ctx = empty_context();
        let diagnoses = engine.diagnose(&ctx);
        assert!(
            diagnoses.is_empty(),
            "Healthy gateway should produce no diagnoses"
        );
    }

    #[test]
    fn test_engine_detects_circuit_breaker_cascade() {
        let engine = DiagnosticsEngine::with_builtin_rules();
        let mut ctx = empty_context();
        ctx.circuit_breakers = vec![
            CircuitBreakerSnapshot {
                backend_id: "10.0.1.1:8080".to_string(),
                state: "Open".to_string(),
                failure_count: 5,
            },
            CircuitBreakerSnapshot {
                backend_id: "10.0.1.2:8080".to_string(),
                state: "Open".to_string(),
                failure_count: 5,
            },
        ];
        ctx.snapshot.open_circuits = 2;

        let diagnoses = engine.diagnose(&ctx);
        assert!(
            diagnoses
                .iter()
                .any(|d| d.rule_id == "RAUTA-CB-001" && d.severity == Severity::Critical),
            "Should detect circuit breaker cascade"
        );
        let cascade = diagnoses
            .iter()
            .find(|d| d.rule_id == "RAUTA-CB-001")
            .expect("cascade diagnosis should be present");
        assert_eq!(cascade.ontology_evidence.len(), 2);
        assert!(cascade
            .ontology_evidence
            .iter()
            .all(|e| e.schema_version == crate::ontology::ONTOLOGY_SCHEMA_VERSION));
    }

    #[test]
    fn test_engine_symptom_filter() {
        let engine = DiagnosticsEngine::with_builtin_rules();
        let mut ctx = empty_context();
        ctx.circuit_breakers = vec![CircuitBreakerSnapshot {
            backend_id: "10.0.1.1:8080".to_string(),
            state: "Open".to_string(),
            failure_count: 5,
        }];

        // Filter by symptom keyword
        let diagnoses = engine.diagnose_symptom(&ctx, "circuit-breaker");
        assert!(
            diagnoses.iter().any(|d| d.rule_id == "RAUTA-CB-002"),
            "Should find CB-002 for single open circuit"
        );
    }

    #[test]
    fn test_engine_detects_no_healthy_backends() {
        let engine = DiagnosticsEngine::with_builtin_rules();
        let mut ctx = empty_context();
        ctx.routes = vec![RouteSnapshot {
            pattern: "/api".to_string(),
            method: "GET".to_string(),
            backends: vec![], // No backends at all
            has_request_filters: false,
            has_response_filters: false,
            has_redirect: false,
            has_timeout: false,
            has_retry: false,
        }];

        let diagnoses = engine.diagnose(&ctx);
        assert!(
            diagnoses
                .iter()
                .any(|d| d.rule_id == "RAUTA-BE-001" && d.severity == Severity::Critical),
            "Should detect no healthy backends"
        );
        let no_backends = diagnoses
            .iter()
            .find(|d| d.rule_id == "RAUTA-BE-001")
            .expect("no-backends diagnosis should be present");
        assert!(no_backends
            .ontology_evidence
            .iter()
            .any(|e| e.subject.kind == crate::ontology::EntityKind::Route));
    }
}
