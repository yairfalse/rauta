//! Local Gateway Query Implementation
//!
//! Reads directly from `Arc<Router>`, `Arc<CircuitBreakerManager>`, and `Arc<RateLimiter>`.
//! Used by the admin server and MCP server when running in-process.

use agent_api::query::GatewayQuery;
use agent_api::types::*;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Instant;

use crate::proxy::circuit_breaker::CircuitBreakerManager;
use crate::proxy::rate_limiter::RateLimiter;
use crate::proxy::router::Router;

/// Local query implementation that reads from shared gateway state
pub struct LocalGatewayQuery {
    router: Arc<Router>,
    circuit_breaker: Arc<CircuitBreakerManager>,
    rate_limiter: Arc<RateLimiter>,
    start_time: Instant,
}

impl LocalGatewayQuery {
    pub fn new(
        router: Arc<Router>,
        circuit_breaker: Arc<CircuitBreakerManager>,
        rate_limiter: Arc<RateLimiter>,
    ) -> Self {
        Self {
            router,
            circuit_breaker,
            rate_limiter,
            start_time: Instant::now(),
        }
    }
}

#[async_trait]
impl GatewayQuery for LocalGatewayQuery {
    async fn snapshot(&self) -> anyhow::Result<GatewaySnapshot> {
        let route_count = self.router.route_count();
        let uptime = self.start_time.elapsed().as_secs();
        let open_circuits = self.circuit_breaker.open_count();
        let exhausted_rate_limiters = self.rate_limiter.exhausted_count();

        Ok(GatewaySnapshot {
            status: "ok".to_string(),
            uptime_seconds: uptime,
            route_count,
            open_circuits,
            exhausted_rate_limiters,
            listeners: vec![],
            cache_stats: None, // LRU cache removed — ArcSwap is faster
        })
    }

    async fn list_routes(
        &self,
        method_filter: Option<&str>,
        path_prefix: Option<&str>,
    ) -> anyhow::Result<Vec<RouteSnapshot>> {
        let mut routes = self.router.list_routes();

        // Apply filters
        if let Some(method) = method_filter {
            let method_upper = method.to_uppercase();
            routes.retain(|r| r.method == method_upper);
        }
        if let Some(prefix) = path_prefix {
            routes.retain(|r| r.pattern.starts_with(prefix));
        }

        // Sort by pattern for stable output
        routes.sort_by(|a, b| a.pattern.cmp(&b.pattern));
        Ok(routes)
    }

    async fn get_route(&self, pattern: &str) -> anyhow::Result<Option<RouteSnapshot>> {
        let routes = self.router.list_routes();
        Ok(routes.into_iter().find(|r| r.pattern == pattern))
    }

    async fn list_circuit_breakers(
        &self,
        state_filter: Option<&str>,
    ) -> anyhow::Result<Vec<CircuitBreakerSnapshot>> {
        let mut breakers = self.circuit_breaker.snapshot_all();

        if let Some(state) = state_filter {
            let state_upper = state.to_uppercase();
            // Match "OPEN", "CLOSED", "HALFOPEN"
            breakers.retain(|b| b.state.to_uppercase() == state_upper);
        }

        breakers.sort_by(|a, b| a.backend_id.cmp(&b.backend_id));
        Ok(breakers)
    }

    async fn list_rate_limiters(
        &self,
        route_filter: Option<&str>,
    ) -> anyhow::Result<Vec<RateLimiterSnapshot>> {
        let mut limiters = self.rate_limiter.snapshot_all();

        if let Some(route) = route_filter {
            limiters.retain(|l| l.route.contains(route));
        }

        limiters.sort_by(|a, b| a.route.cmp(&b.route));
        Ok(limiters)
    }

    async fn list_listeners(&self) -> anyhow::Result<Vec<ListenerSnapshot>> {
        // ListenerManager is not wired into LocalGatewayQuery yet
        Ok(vec![])
    }

    async fn cache_stats(&self) -> anyhow::Result<Option<CacheStats>> {
        // LRU cache was removed — ArcSwap route snapshot is faster.
        // Return None to signal caching is not applicable.
        Ok(None)
    }

    async fn metrics_snapshot(
        &self,
        _metric_filter: Option<&str>,
    ) -> anyhow::Result<Vec<MetricSnapshot>> {
        Ok(vec![])
    }

    async fn diagnose(
        &self,
        symptom: &str,
        _route_filter: Option<&str>,
        _backend_filter: Option<&str>,
    ) -> anyhow::Result<Vec<Diagnosis>> {
        use agent_api::diagnostics::engine::{DiagnosticContext, DiagnosticsEngine};

        let snapshot = self.snapshot().await?;
        let routes = self.router.list_routes();
        let circuit_breakers = self.circuit_breaker.snapshot_all();
        let rate_limiters = self.rate_limiter.snapshot_all();

        let ctx = DiagnosticContext {
            snapshot,
            routes,
            circuit_breakers,
            rate_limiters,
        };

        let engine = DiagnosticsEngine::with_builtin_rules();
        Ok(engine.diagnose_symptom(&ctx, symptom))
    }

    async fn drain_backend(
        &self,
        _backend: &str,
        _timeout_secs: Option<u64>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("drain_backend not yet implemented via admin API")
    }

    async fn undrain_backend(&self, _backend: &str) -> anyhow::Result<()> {
        anyhow::bail!("undrain_backend not yet implemented via admin API")
    }
}
