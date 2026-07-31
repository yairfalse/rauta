//! Built-In Diagnostic Rules
//!
//! Deterministic rules that detect common gateway issues.
//! Each rule produces structured `Diagnosis` with causal chain, evidence, and suggested actions.

use crate::diagnostics::engine::{DiagnosticContext, DiagnosticRule};
use crate::ontology::{EntityKind, EntityRef, EvidenceKind, OntologyEvidence};
use crate::types::{Diagnosis, Severity, SuggestedAction};

fn backend_entity(id: impl Into<String>) -> EntityRef {
    EntityRef::new(EntityKind::Backend, id)
}

fn route_entity(method: &str, pattern: &str) -> EntityRef {
    EntityRef::new(EntityKind::Route, format!("{} {}", method, pattern))
}

fn circuit_breaker_evidence(
    rule_id: &str,
    backend_id: &str,
    state: &str,
    failure_count: u32,
) -> OntologyEvidence {
    OntologyEvidence::new(
        format!("{}:{}:circuit-breaker", rule_id, backend_id),
        EvidenceKind::CircuitBreakerState,
        backend_entity(backend_id),
        format!(
            "Backend {} circuit breaker is {} with {} failures",
            backend_id, state, failure_count
        ),
    )
    .with_attr("state", state)
    .with_attr("failure_count", failure_count)
}

/// RAUTA-CB-001: Circuit breaker cascade (≥2 circuits Open simultaneously)
pub struct CircuitBreakerCascade;

impl DiagnosticRule for CircuitBreakerCascade {
    fn id(&self) -> &str {
        "RAUTA-CB-001"
    }

    fn symptom(&self) -> &str {
        "circuit-breaker-cascade"
    }

    fn evaluate(&self, ctx: &DiagnosticContext) -> Vec<Diagnosis> {
        let open_breakers: Vec<_> = ctx
            .circuit_breakers
            .iter()
            .filter(|cb| cb.state == "Open")
            .collect();

        if open_breakers.len() >= 2 {
            vec![Diagnosis {
                rule_id: self.id().to_string(),
                symptom: self.symptom().to_string(),
                severity: Severity::Critical,
                confidence: 0.95,
                causal_chain: vec![
                    format!(
                        "{} backends have open circuit breakers",
                        open_breakers.len()
                    ),
                    "Multiple simultaneous failures suggest upstream dependency issue".to_string(),
                ],
                evidence: open_breakers
                    .iter()
                    .map(|cb| {
                        format!(
                            "Backend {} is Open (failures: {})",
                            cb.backend_id, cb.failure_count
                        )
                    })
                    .collect(),
                ontology_evidence: open_breakers
                    .iter()
                    .map(|cb| {
                        circuit_breaker_evidence(
                            self.id(),
                            &cb.backend_id,
                            &cb.state,
                            cb.failure_count,
                        )
                    })
                    .collect(),
                suggested_actions: vec![
                    SuggestedAction {
                        description: "Check if backends share a common upstream dependency"
                            .to_string(),
                        cli_command: Some("rauta backends health".to_string()),
                    },
                    SuggestedAction {
                        description: "Check network connectivity to backend cluster".to_string(),
                        cli_command: None,
                    },
                    SuggestedAction {
                        description: "Review circuit breaker thresholds if false positives"
                            .to_string(),
                        cli_command: Some("rauta diagnose circuit-breaker-open".to_string()),
                    },
                ],
            }]
        } else {
            vec![]
        }
    }
}

/// RAUTA-CB-002: Single circuit breaker open
pub struct CircuitBreakerOpen;

impl DiagnosticRule for CircuitBreakerOpen {
    fn id(&self) -> &str {
        "RAUTA-CB-002"
    }

    fn symptom(&self) -> &str {
        "circuit-breaker-open"
    }

    fn evaluate(&self, ctx: &DiagnosticContext) -> Vec<Diagnosis> {
        ctx.circuit_breakers
            .iter()
            .filter(|cb| cb.state == "Open")
            .map(|cb| Diagnosis {
                rule_id: self.id().to_string(),
                symptom: self.symptom().to_string(),
                severity: Severity::Warning,
                confidence: 0.9,
                causal_chain: vec![
                    format!(
                        "Backend {} circuit breaker is Open after {} failures",
                        cb.backend_id, cb.failure_count
                    ),
                    "Traffic is being rerouted to healthy backends".to_string(),
                ],
                evidence: vec![format!(
                    "Circuit state: {}, failures: {}",
                    cb.state, cb.failure_count
                )],
                ontology_evidence: vec![circuit_breaker_evidence(
                    self.id(),
                    &cb.backend_id,
                    &cb.state,
                    cb.failure_count,
                )],
                suggested_actions: vec![
                    SuggestedAction {
                        description: format!("Check backend {} health and logs", cb.backend_id),
                        cli_command: Some(format!(
                            "rauta backends health --backend={}",
                            cb.backend_id
                        )),
                    },
                    SuggestedAction {
                        description: "Wait for circuit breaker timeout to attempt recovery"
                            .to_string(),
                        cli_command: None,
                    },
                ],
            })
            .collect()
    }
}

/// RAUTA-RL-001: Rate limit exhausted
pub struct RateLimitExhausted;

impl DiagnosticRule for RateLimitExhausted {
    fn id(&self) -> &str {
        "RAUTA-RL-001"
    }

    fn symptom(&self) -> &str {
        "rate-limit-exhausted"
    }

    fn evaluate(&self, ctx: &DiagnosticContext) -> Vec<Diagnosis> {
        ctx.rate_limiters
            .iter()
            .filter(|rl| rl.tokens_available <= 0.0)
            .map(|rl| Diagnosis {
                rule_id: self.id().to_string(),
                symptom: self.symptom().to_string(),
                severity: Severity::Warning,
                confidence: 0.85,
                causal_chain: vec![
                    format!("Route {} has exhausted its rate limit", rl.route),
                    format!(
                        "Configured at {:.0} req/s with burst capacity {:.0}",
                        rl.refill_rate, rl.capacity
                    ),
                ],
                evidence: vec![format!(
                    "Tokens available: {:.1}/{:.0}",
                    rl.tokens_available, rl.capacity
                )],
                ontology_evidence: vec![OntologyEvidence::new(
                    format!("{}:{}:rate-limiter", self.id(), rl.route),
                    EvidenceKind::RateLimiterState,
                    EntityRef::new(EntityKind::RateLimiter, rl.route.clone()),
                    format!(
                        "Route {} has {:.1}/{:.0} tokens available",
                        rl.route, rl.tokens_available, rl.capacity
                    ),
                )
                .with_attr("route", rl.route.clone())
                .with_attr("tokens_available", rl.tokens_available)
                .with_attr("capacity", rl.capacity)
                .with_attr("refill_rate", rl.refill_rate)],
                suggested_actions: vec![
                    SuggestedAction {
                        description: "Review if rate limit is appropriate for current traffic"
                            .to_string(),
                        cli_command: Some(format!(
                            "rauta metrics query rauta_rate_limit_requests_total{{route=\"{}\"}}",
                            rl.route
                        )),
                    },
                    SuggestedAction {
                        description: "Consider increasing rate limit or burst capacity".to_string(),
                        cli_command: None,
                    },
                ],
            })
            .collect()
    }
}

/// RAUTA-BE-001: No healthy backends for a route
pub struct NoHealthyBackends;

impl DiagnosticRule for NoHealthyBackends {
    fn id(&self) -> &str {
        "RAUTA-BE-001"
    }

    fn symptom(&self) -> &str {
        "no-healthy-backends"
    }

    fn evaluate(&self, ctx: &DiagnosticContext) -> Vec<Diagnosis> {
        ctx.routes
            .iter()
            .filter(|route| {
                route.backends.is_empty()
                    || route
                        .backends
                        .iter()
                        .all(|b| b.health_score.is_some_and(|s| s < 0.5))
            })
            .map(|route| {
                let backend_count = route.backends.len();
                Diagnosis {
                    rule_id: self.id().to_string(),
                    symptom: self.symptom().to_string(),
                    severity: Severity::Critical,
                    confidence: 0.95,
                    causal_chain: vec![
                        format!(
                            "Route {} {} has no healthy backends",
                            route.method, route.pattern
                        ),
                        if backend_count == 0 {
                            "No backends configured for this route".to_string()
                        } else {
                            format!("All {} backends are unhealthy", backend_count)
                        },
                    ],
                    evidence: route
                        .backends
                        .iter()
                        .map(|b| {
                            format!(
                                "Backend {}:{} health_score={:?} draining={}",
                                b.address, b.port, b.health_score, b.is_draining
                            )
                        })
                        .collect(),
                    ontology_evidence: if route.backends.is_empty() {
                        vec![OntologyEvidence::new(
                            format!("{}:{}:route-backends", self.id(), route.pattern),
                            EvidenceKind::RouteState,
                            route_entity(&route.method, &route.pattern),
                            format!("Route {} {} has no backends", route.method, route.pattern),
                        )
                        .with_attr("backend_count", backend_count)]
                    } else {
                        route
                            .backends
                            .iter()
                            .map(|backend| {
                                OntologyEvidence::new(
                                    format!(
                                        "{}:{}:{}:{}:backend-health",
                                        self.id(),
                                        route.pattern,
                                        backend.address,
                                        backend.port
                                    ),
                                    EvidenceKind::BackendHealth,
                                    backend_entity(format!("{}:{}", backend.address, backend.port)),
                                    format!(
                                        "Backend {}:{} health_score={:?} draining={}",
                                        backend.address,
                                        backend.port,
                                        backend.health_score,
                                        backend.is_draining
                                    ),
                                )
                                .with_attr("route", route.pattern.clone())
                                .with_attr("health_score", backend.health_score.unwrap_or(0.0))
                                .with_attr("is_draining", backend.is_draining)
                            })
                            .collect()
                    },
                    suggested_actions: vec![
                        SuggestedAction {
                            description: "Check backend pod status in Kubernetes".to_string(),
                            cli_command: Some(format!("rauta routes get {}", route.pattern)),
                        },
                        SuggestedAction {
                            description: "Verify EndpointSlice has ready addresses".to_string(),
                            cli_command: None,
                        },
                    ],
                }
            })
            .collect()
    }
}

/// RAUTA-BE-002: All backends draining
pub struct AllBackendsDraining;

impl DiagnosticRule for AllBackendsDraining {
    fn id(&self) -> &str {
        "RAUTA-BE-002"
    }

    fn symptom(&self) -> &str {
        "all-backends-draining"
    }

    fn evaluate(&self, ctx: &DiagnosticContext) -> Vec<Diagnosis> {
        ctx.routes
            .iter()
            .filter(|route| {
                !route.backends.is_empty() && route.backends.iter().all(|b| b.is_draining)
            })
            .map(|route| Diagnosis {
                rule_id: self.id().to_string(),
                symptom: self.symptom().to_string(),
                severity: Severity::Warning,
                confidence: 0.9,
                causal_chain: vec![
                    format!(
                        "Route {} {} has all backends draining",
                        route.method, route.pattern
                    ),
                    "No backends available to serve new requests".to_string(),
                ],
                evidence: route
                    .backends
                    .iter()
                    .map(|b| format!("Backend {}:{} is draining", b.address, b.port))
                    .collect(),
                ontology_evidence: route
                    .backends
                    .iter()
                    .map(|backend| {
                        OntologyEvidence::new(
                            format!(
                                "{}:{}:{}:{}:draining",
                                self.id(),
                                route.pattern,
                                backend.address,
                                backend.port
                            ),
                            EvidenceKind::BackendDrainState,
                            backend_entity(format!("{}:{}", backend.address, backend.port)),
                            format!("Backend {}:{} is draining", backend.address, backend.port),
                        )
                        .with_attr("route", route.pattern.clone())
                        .with_attr("is_draining", backend.is_draining)
                    })
                    .collect(),
                suggested_actions: vec![SuggestedAction {
                    description: "Ensure replacement backends are being provisioned".to_string(),
                    cli_command: Some("rauta backends health".to_string()),
                }],
            })
            .collect()
    }
}

/// RAUTA-CACHE-001: Low cache hit rate
pub struct LowCacheHitRate;

impl DiagnosticRule for LowCacheHitRate {
    fn id(&self) -> &str {
        "RAUTA-CACHE-001"
    }

    fn symptom(&self) -> &str {
        "low-cache-hit-rate"
    }

    fn evaluate(&self, ctx: &DiagnosticContext) -> Vec<Diagnosis> {
        if let Some(cache) = &ctx.snapshot.cache_stats {
            let total = cache.hits + cache.misses;
            if total >= 1000 && cache.hit_rate < 0.5 {
                return vec![Diagnosis {
                    rule_id: self.id().to_string(),
                    symptom: self.symptom().to_string(),
                    severity: Severity::Info,
                    confidence: 0.7,
                    causal_chain: vec![
                        format!(
                            "Route cache hit rate is {:.1}% ({} hits, {} misses)",
                            cache.hit_rate * 100.0,
                            cache.hits,
                            cache.misses
                        ),
                        "Many unique paths are being requested, reducing cache effectiveness"
                            .to_string(),
                    ],
                    evidence: vec![format!(
                        "Cache size: {}, hit_rate: {:.1}%",
                        cache.size,
                        cache.hit_rate * 100.0
                    )],
                    ontology_evidence: vec![OntologyEvidence::new(
                        format!("{}:route-cache", self.id()),
                        EvidenceKind::CacheState,
                        EntityRef::new(EntityKind::Cache, "route-cache"),
                        format!(
                            "Route cache hit rate is {:.1}% with {} hits and {} misses",
                            cache.hit_rate * 100.0,
                            cache.hits,
                            cache.misses
                        ),
                    )
                    .with_attr("hits", cache.hits)
                    .with_attr("misses", cache.misses)
                    .with_attr("size", cache.size)
                    .with_attr("hit_rate", cache.hit_rate)],
                    suggested_actions: vec![SuggestedAction {
                        description:
                            "Consider if path cardinality is expected (API versioning, UUIDs in paths)"
                                .to_string(),
                        cli_command: Some("rauta traffic top".to_string()),
                    }],
                }];
            }
        }
        vec![]
    }
}

/// RAUTA-LISTEN-001: Listener conflict (multiple listeners on same port)
pub struct ListenerConflict;

impl DiagnosticRule for ListenerConflict {
    fn id(&self) -> &str {
        "RAUTA-LISTEN-001"
    }

    fn symptom(&self) -> &str {
        "listener-conflict"
    }

    fn evaluate(&self, ctx: &DiagnosticContext) -> Vec<Diagnosis> {
        use std::collections::HashMap;

        let mut port_counts: HashMap<u16, Vec<&str>> = HashMap::new();
        for listener in &ctx.snapshot.listeners {
            port_counts
                .entry(listener.port)
                .or_default()
                .push(&listener.protocol);
        }

        port_counts
            .iter()
            .filter(|(_, protocols)| protocols.len() > 1)
            .map(|(port, protocols)| Diagnosis {
                rule_id: self.id().to_string(),
                symptom: self.symptom().to_string(),
                severity: Severity::Warning,
                confidence: 0.8,
                causal_chain: vec![
                    format!(
                        "Port {} has {} listener configurations",
                        port,
                        protocols.len()
                    ),
                    "Multiple Gateway resources may be competing for the same port".to_string(),
                ],
                evidence: protocols
                    .iter()
                    .map(|p| format!("Protocol: {} on port {}", p, port))
                    .collect(),
                ontology_evidence: protocols
                    .iter()
                    .map(|protocol| {
                        OntologyEvidence::new(
                            format!("{}:{}:{}:listener", self.id(), port, protocol),
                            EvidenceKind::ListenerState,
                            EntityRef::new(EntityKind::Listener, format!("{}:{}", protocol, port)),
                            format!("Protocol: {} on port {}", protocol, port),
                        )
                        .with_attr("port", *port as u64)
                        .with_attr("protocol", (*protocol).to_string())
                    })
                    .collect(),
                suggested_actions: vec![SuggestedAction {
                    description: "Review Gateway resources for port conflicts".to_string(),
                    cli_command: Some("rauta status".to_string()),
                }],
            })
            .collect()
    }
}
