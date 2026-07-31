//! MCP Server Handler (rmcp)
//!
//! Implements `rmcp::ServerHandler` for RAUTA gateway tools.
//! Wraps any `GatewayQuery` implementation — `LocalGatewayQuery` for in-process
//! access, `RemoteGatewayQuery` for HTTP-based access.
//!
//! Usage:
//! ```rust,ignore
//! let handler = RautaMcpHandler::new(query);
//! rmcp::ServiceExt::serve(handler, rmcp::transport::stdio()).await?;
//! ```

use agent_api::query::GatewayQuery;
use agent_api::temporal::TemporalQuery;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ============================================================================
// Parameter types for MCP tools
// These use rmcp's schemars (1.x) for schema generation in tool definitions
// ============================================================================

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListRoutesParams {
    /// Filter by HTTP method (GET, POST, etc.)
    pub method: Option<String>,
    /// Filter by path prefix
    pub path_prefix: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetRouteParams {
    /// Route pattern to look up
    pub pattern: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListCircuitBreakersParams {
    /// Filter by state: Open, Closed, or HalfOpen
    pub state: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListRateLimitersParams {
    /// Filter by route pattern
    pub route: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiagnoseParams {
    /// Symptom to diagnose (e.g., "high-latency", "circuit-breaker-cascade")
    pub symptom: String,
    /// Filter by route pattern
    pub route: Option<String>,
    /// Filter by backend address
    pub backend: Option<String>,
    /// Include temporal evidence from the last N seconds
    pub since_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DrainBackendParams {
    /// Backend address to drain (e.g., "10.0.1.5:8080")
    pub backend: String,
    /// Drain timeout in seconds (default: 30)
    pub timeout: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UndrainBackendParams {
    /// Backend address to undrain
    pub backend: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct QuarantineBackendParams {
    /// Backend address to quarantine
    pub backend: String,
    /// Quarantine TTL in seconds
    pub ttl_secs: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MetricsSnapshotParams {
    /// Filter by metric name
    pub metric: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TemporalParams {
    /// Include history from the last N seconds
    pub since_seconds: Option<u64>,
    /// Include events at or after this sequence
    pub from_sequence: Option<u64>,
    /// Include events at or before this sequence
    pub to_sequence: Option<u64>,
}

// ============================================================================
// MCP Handler
// ============================================================================

/// RAUTA MCP server handler
///
/// Each `#[tool]` method maps to a `GatewayQuery` method.
/// rmcp generates JSON Schema from the parameter types and handles
/// JSON-RPC framing over stdio.
#[derive(Clone)]
pub struct RautaMcpHandler {
    query: Arc<dyn GatewayQuery>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl RautaMcpHandler {
    pub fn new(query: Arc<dyn GatewayQuery>) -> Self {
        Self {
            query,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Get gateway health overview: uptime, route count, open circuits, rate limiter status"
    )]
    async fn rauta_status(&self) -> Result<CallToolResult, McpError> {
        let snapshot = self
            .query
            .snapshot()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "List all configured routes with backends, filters, and health status")]
    async fn rauta_list_routes(
        &self,
        Parameters(params): Parameters<ListRoutesParams>,
    ) -> Result<CallToolResult, McpError> {
        let routes = self
            .query
            .list_routes(params.method.as_deref(), params.path_prefix.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&routes)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "Get detailed information about a single route")]
    async fn rauta_get_route(
        &self,
        Parameters(params): Parameters<GetRouteParams>,
    ) -> Result<CallToolResult, McpError> {
        let route = self
            .query
            .get_route(&params.pattern)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&route)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "List circuit breaker states for all backends")]
    async fn rauta_list_circuit_breakers(
        &self,
        Parameters(params): Parameters<ListCircuitBreakersParams>,
    ) -> Result<CallToolResult, McpError> {
        let breakers = self
            .query
            .list_circuit_breakers(params.state.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&breakers)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "List rate limiter states showing tokens available and capacity")]
    async fn rauta_list_rate_limiters(
        &self,
        Parameters(params): Parameters<ListRateLimitersParams>,
    ) -> Result<CallToolResult, McpError> {
        let limiters = self
            .query
            .list_rate_limiters(params.route.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&limiters)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(
        description = "Run diagnostic rules to detect gateway issues. Returns structured diagnoses with causal chains and suggested actions"
    )]
    async fn rauta_diagnose(
        &self,
        Parameters(params): Parameters<DiagnoseParams>,
    ) -> Result<CallToolResult, McpError> {
        let diagnoses = self
            .query
            .diagnose_since(
                &params.symptom,
                params.route.as_deref(),
                params.backend.as_deref(),
                params.since_seconds,
            )
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&diagnoses)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "Get recent bounded gateway timeline events and snapshots")]
    async fn rauta_timeline(
        &self,
        Parameters(params): Parameters<TemporalParams>,
    ) -> Result<CallToolResult, McpError> {
        let timeline = self
            .query
            .timeline(temporal_query(params))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&timeline)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "Diff recent gateway state over a bounded time window")]
    async fn rauta_diff(
        &self,
        Parameters(params): Parameters<TemporalParams>,
    ) -> Result<CallToolResult, McpError> {
        let diff = self
            .query
            .diff(temporal_query(params))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&diff)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "Get route cache statistics (hit rate, size)")]
    async fn rauta_cache_stats(&self) -> Result<CallToolResult, McpError> {
        let stats = self
            .query
            .cache_stats()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&stats)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "List active network listeners with protocols and Gateway references")]
    async fn rauta_list_listeners(&self) -> Result<CallToolResult, McpError> {
        let listeners = self
            .query
            .list_listeners()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&listeners)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "Get Prometheus metrics as structured JSON")]
    async fn rauta_metrics_snapshot(
        &self,
        Parameters(params): Parameters<MetricsSnapshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let metrics = self
            .query
            .metrics_snapshot(params.metric.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&metrics)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(
        description = "Gracefully drain a backend, preventing new requests while allowing existing connections to finish"
    )]
    async fn rauta_drain_backend(
        &self,
        Parameters(params): Parameters<DrainBackendParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .query
            .drain_backend(&params.backend, params.timeout)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "Cancel drain for a backend, restoring it to active service")]
    async fn rauta_undrain_backend(
        &self,
        Parameters(params): Parameters<UndrainBackendParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .query
            .undrain_backend(&params.backend)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "Quarantine a backend for a bounded TTL with rollback metadata")]
    async fn rauta_quarantine_backend(
        &self,
        Parameters(params): Parameters<QuarantineBackendParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .query
            .quarantine_backend(&params.backend, params.ttl_secs)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

fn temporal_query(params: TemporalParams) -> TemporalQuery {
    TemporalQuery {
        since_seconds: params.since_seconds,
        from_sequence: params.from_sequence,
        to_sequence: params.to_sequence,
    }
}

#[tool_handler]
impl ServerHandler for RautaMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "RAUTA AI-native Kubernetes API gateway. Query routes, backends, \
                 circuit breakers, rate limiters, and run diagnostics."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_api::actions::{
        ActionEvidence, ActionPrecondition, ActionResult, ActionRisk, ActionStatus,
        RollbackMetadata,
    };
    use agent_api::ontology::{ActionKind, EntityKind, EntityRef};
    use agent_api::temporal::{GatewayDiff, TimelineSnapshot};
    use agent_api::types::{
        BackendSnapshot, CacheStats, CircuitBreakerSnapshot, Diagnosis, GatewaySnapshot,
        ListenerSnapshot, MetricSnapshot, MetricValue, RateLimiterSnapshot, RouteSnapshot,
        Severity,
    };
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeGatewayQuery {
        calls: Mutex<Vec<&'static str>>,
    }

    impl FakeGatewayQuery {
        fn record(&self, call: &'static str) {
            self.calls.lock().expect("calls lock poisoned").push(call);
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().expect("calls lock poisoned").clone()
        }
    }

    #[async_trait]
    impl GatewayQuery for FakeGatewayQuery {
        async fn snapshot(&self) -> anyhow::Result<GatewaySnapshot> {
            self.record("snapshot");
            Ok(GatewaySnapshot {
                status: "ok".to_string(),
                uptime_seconds: 42,
                route_count: 1,
                open_circuits: 0,
                exhausted_rate_limiters: 0,
                listeners: vec![ListenerSnapshot {
                    port: 8080,
                    protocol: "HTTP".to_string(),
                    gateway_refs: vec!["default/gateway".to_string()],
                }],
                cache_stats: Some(CacheStats {
                    hits: 9,
                    misses: 1,
                    size: 1,
                    hit_rate: 0.9,
                }),
            })
        }

        async fn list_routes(
            &self,
            method_filter: Option<&str>,
            path_prefix: Option<&str>,
        ) -> anyhow::Result<Vec<RouteSnapshot>> {
            self.record("list_routes");
            assert_eq!(method_filter, Some("GET"));
            assert_eq!(path_prefix, Some("/api"));
            Ok(vec![sample_route()])
        }

        async fn get_route(&self, pattern: &str) -> anyhow::Result<Option<RouteSnapshot>> {
            self.record("get_route");
            assert_eq!(pattern, "/api");
            Ok(Some(sample_route()))
        }

        async fn list_circuit_breakers(
            &self,
            state_filter: Option<&str>,
        ) -> anyhow::Result<Vec<CircuitBreakerSnapshot>> {
            self.record("list_circuit_breakers");
            assert_eq!(state_filter, Some("Open"));
            Ok(vec![CircuitBreakerSnapshot {
                backend_id: "10.0.0.1:8080".to_string(),
                state: "Open".to_string(),
                failure_count: 5,
            }])
        }

        async fn list_rate_limiters(
            &self,
            route_filter: Option<&str>,
        ) -> anyhow::Result<Vec<RateLimiterSnapshot>> {
            self.record("list_rate_limiters");
            assert_eq!(route_filter, Some("/api"));
            Ok(vec![RateLimiterSnapshot {
                route: "/api".to_string(),
                tokens_available: 5.0,
                capacity: 10.0,
                refill_rate: 1.0,
            }])
        }

        async fn list_listeners(&self) -> anyhow::Result<Vec<ListenerSnapshot>> {
            self.record("list_listeners");
            Ok(vec![ListenerSnapshot {
                port: 8080,
                protocol: "HTTP".to_string(),
                gateway_refs: vec!["default/gateway".to_string()],
            }])
        }

        async fn cache_stats(&self) -> anyhow::Result<Option<CacheStats>> {
            self.record("cache_stats");
            Ok(Some(CacheStats {
                hits: 9,
                misses: 1,
                size: 1,
                hit_rate: 0.9,
            }))
        }

        async fn metrics_snapshot(
            &self,
            metric_filter: Option<&str>,
        ) -> anyhow::Result<Vec<MetricSnapshot>> {
            self.record("metrics_snapshot");
            assert_eq!(metric_filter, Some("http_requests_total"));
            Ok(vec![MetricSnapshot {
                name: "http_requests_total".to_string(),
                help: "Total requests".to_string(),
                metric_type: "counter".to_string(),
                values: vec![MetricValue {
                    labels: HashMap::new(),
                    value: 1.0,
                }],
            }])
        }

        async fn timeline(&self, query: TemporalQuery) -> anyhow::Result<TimelineSnapshot> {
            self.record("timeline");
            assert_eq!(query.since_seconds, Some(60));
            Ok(TimelineSnapshot {
                retention: 64,
                generated_at_unix_ms: 1,
                events: vec![],
                snapshots: vec![],
            })
        }

        async fn diff(&self, query: TemporalQuery) -> anyhow::Result<GatewayDiff> {
            self.record("diff");
            assert_eq!(query.since_seconds, Some(60));
            Ok(GatewayDiff {
                from_sequence: Some(1),
                to_sequence: 1,
                since_seconds: Some(60),
                route_count_delta: 0,
                open_circuits_delta: 0,
                exhausted_rate_limiters_delta: 0,
                added_routes: vec![],
                removed_routes: vec![],
                changed_routes: vec![],
                added_listeners: vec![],
                removed_listeners: vec![],
                changed_circuit_breakers: vec![],
                changed_rate_limiters: vec![],
                events: vec![],
            })
        }

        async fn diagnose(
            &self,
            symptom: &str,
            route_filter: Option<&str>,
            backend_filter: Option<&str>,
        ) -> anyhow::Result<Vec<Diagnosis>> {
            self.record("diagnose");
            assert_eq!(symptom, "no-healthy-backends");
            assert_eq!(route_filter, Some("/api"));
            assert_eq!(backend_filter, Some("10.0.0.1:8080"));
            Ok(vec![Diagnosis {
                rule_id: "RAUTA-BE-001".to_string(),
                symptom: symptom.to_string(),
                severity: Severity::Critical,
                confidence: 0.95,
                causal_chain: vec!["No healthy backends".to_string()],
                evidence: vec!["Backend unhealthy".to_string()],
                ontology_evidence: vec![],
                suggested_actions: vec![],
            }])
        }

        async fn diagnose_since(
            &self,
            symptom: &str,
            route_filter: Option<&str>,
            backend_filter: Option<&str>,
            since_seconds: Option<u64>,
        ) -> anyhow::Result<Vec<Diagnosis>> {
            self.record("diagnose_since");
            assert_eq!(since_seconds, Some(60));
            self.diagnose(symptom, route_filter, backend_filter).await
        }

        async fn drain_backend(
            &self,
            backend: &str,
            timeout_secs: Option<u64>,
        ) -> anyhow::Result<ActionResult> {
            self.record("drain_backend");
            assert_eq!(backend, "10.0.0.1:8080");
            assert_eq!(timeout_secs, Some(30));
            Ok(sample_action(ActionKind::DrainBackend, backend))
        }

        async fn undrain_backend(&self, backend: &str) -> anyhow::Result<ActionResult> {
            self.record("undrain_backend");
            assert_eq!(backend, "10.0.0.1:8080");
            Ok(sample_action(ActionKind::UndrainBackend, backend))
        }

        async fn quarantine_backend(
            &self,
            backend: &str,
            ttl_secs: u64,
        ) -> anyhow::Result<ActionResult> {
            self.record("quarantine_backend");
            assert_eq!(backend, "10.0.0.1:8080");
            assert_eq!(ttl_secs, 300);
            Ok(sample_action(ActionKind::QuarantineBackend, backend))
        }
    }

    fn sample_action(kind: ActionKind, backend: &str) -> ActionResult {
        ActionResult {
            action_id: "action-1".to_string(),
            kind: kind.clone(),
            target: EntityRef::new(EntityKind::Backend, backend),
            status: ActionStatus::Applied,
            risk: ActionRisk::Medium,
            preconditions: vec![ActionPrecondition {
                name: "backend_present".to_string(),
                passed: true,
                message: "Backend is referenced by at least one route".to_string(),
            }],
            before: ActionEvidence {
                backend: backend.to_string(),
                was_draining: false,
                is_draining: false,
                affected_routes: vec!["/api".to_string()],
            },
            after: ActionEvidence {
                backend: backend.to_string(),
                was_draining: true,
                is_draining: true,
                affected_routes: vec!["/api".to_string()],
            },
            rollback: Some(RollbackMetadata {
                action: ActionKind::UndrainBackend,
                cli_command: format!("rauta backends undrain {}", backend),
                reason: "test rollback".to_string(),
            }),
            expires_at_unix_ms: Some(1),
            timeline_event: agent_api::temporal::TemporalEvent {
                sequence: 1,
                timestamp_unix_ms: 1,
                kind: agent_api::temporal::TemporalEventKind::AdminAction,
                subject: EntityRef::new(EntityKind::Backend, backend),
                summary: "test action".to_string(),
                attributes: std::collections::BTreeMap::new(),
            },
        }
    }

    fn sample_route() -> RouteSnapshot {
        RouteSnapshot {
            pattern: "/api".to_string(),
            method: "GET".to_string(),
            backends: vec![BackendSnapshot {
                address: "10.0.0.1".to_string(),
                port: 8080,
                weight: 100,
                is_ipv6: false,
                is_draining: false,
                health_score: Some(1.0),
            }],
            has_request_filters: false,
            has_response_filters: false,
            has_redirect: false,
            has_timeout: false,
            has_retry: false,
        }
    }

    fn handler_with_fake_query() -> (RautaMcpHandler, Arc<FakeGatewayQuery>) {
        let query = Arc::new(FakeGatewayQuery::default());
        let handler = RautaMcpHandler::new(query.clone());
        (handler, query)
    }

    fn result_text(result: CallToolResult) -> String {
        serde_json::to_value(result.content)
            .expect("content should serialize")
            .to_string()
    }

    #[tokio::test]
    async fn status_tool_queries_snapshot() {
        let (handler, query) = handler_with_fake_query();
        let result = handler.rauta_status().await.expect("status succeeds");

        assert_eq!(query.calls(), vec!["snapshot"]);
        let text = result_text(result);
        assert!(text.contains("route_count"));
        assert!(text.contains('1'));
    }

    #[tokio::test]
    async fn route_tools_query_expected_methods() {
        let (handler, query) = handler_with_fake_query();

        handler
            .rauta_list_routes(Parameters(ListRoutesParams {
                method: Some("GET".to_string()),
                path_prefix: Some("/api".to_string()),
            }))
            .await
            .expect("list routes succeeds");
        handler
            .rauta_get_route(Parameters(GetRouteParams {
                pattern: "/api".to_string(),
            }))
            .await
            .expect("get route succeeds");

        assert_eq!(query.calls(), vec!["list_routes", "get_route"]);
    }

    #[tokio::test]
    async fn health_state_tools_query_expected_methods() {
        let (handler, query) = handler_with_fake_query();

        handler
            .rauta_list_circuit_breakers(Parameters(ListCircuitBreakersParams {
                state: Some("Open".to_string()),
            }))
            .await
            .expect("list circuit breakers succeeds");
        handler
            .rauta_list_rate_limiters(Parameters(ListRateLimitersParams {
                route: Some("/api".to_string()),
            }))
            .await
            .expect("list rate limiters succeeds");
        handler
            .rauta_cache_stats()
            .await
            .expect("cache stats succeeds");
        handler
            .rauta_list_listeners()
            .await
            .expect("list listeners succeeds");

        assert_eq!(
            query.calls(),
            vec![
                "list_circuit_breakers",
                "list_rate_limiters",
                "cache_stats",
                "list_listeners"
            ]
        );
    }

    #[tokio::test]
    async fn diagnostics_metrics_and_actions_query_expected_methods() {
        let (handler, query) = handler_with_fake_query();

        handler
            .rauta_diagnose(Parameters(DiagnoseParams {
                symptom: "no-healthy-backends".to_string(),
                route: Some("/api".to_string()),
                backend: Some("10.0.0.1:8080".to_string()),
                since_seconds: Some(60),
            }))
            .await
            .expect("diagnose succeeds");
        handler
            .rauta_metrics_snapshot(Parameters(MetricsSnapshotParams {
                metric: Some("http_requests_total".to_string()),
            }))
            .await
            .expect("metrics succeeds");
        handler
            .rauta_drain_backend(Parameters(DrainBackendParams {
                backend: "10.0.0.1:8080".to_string(),
                timeout: Some(30),
            }))
            .await
            .expect("drain succeeds");
        handler
            .rauta_undrain_backend(Parameters(UndrainBackendParams {
                backend: "10.0.0.1:8080".to_string(),
            }))
            .await
            .expect("undrain succeeds");
        handler
            .rauta_quarantine_backend(Parameters(QuarantineBackendParams {
                backend: "10.0.0.1:8080".to_string(),
                ttl_secs: 300,
            }))
            .await
            .expect("quarantine succeeds");

        assert_eq!(
            query.calls(),
            vec![
                "diagnose_since",
                "diagnose",
                "metrics_snapshot",
                "drain_backend",
                "undrain_backend",
                "quarantine_backend"
            ]
        );
    }

    #[tokio::test]
    async fn temporal_tools_query_expected_methods() {
        let (handler, query) = handler_with_fake_query();

        let params = TemporalParams {
            since_seconds: Some(60),
            from_sequence: None,
            to_sequence: None,
        };
        handler
            .rauta_timeline(Parameters(params))
            .await
            .expect("timeline succeeds");
        handler
            .rauta_diff(Parameters(TemporalParams {
                since_seconds: Some(60),
                from_sequence: None,
                to_sequence: None,
            }))
            .await
            .expect("diff succeeds");

        assert_eq!(query.calls(), vec!["timeline", "diff"]);
    }
}
