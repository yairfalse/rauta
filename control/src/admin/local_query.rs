//! Local Gateway Query Implementation
//!
//! Reads directly from `Arc<Router>`, `Arc<CircuitBreakerManager>`, and `Arc<RateLimiter>`.
//! Used by the admin server and MCP server when running in-process.

use agent_api::query::GatewayQuery;
use agent_api::types::*;
use async_trait::async_trait;
use prometheus::proto::{Metric, MetricFamily, MetricType};
use std::collections::HashMap;
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
            cache_stats: Some(self.cache_stats_internal()),
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
        Ok(Some(self.cache_stats_internal()))
    }

    async fn metrics_snapshot(
        &self,
        metric_filter: Option<&str>,
    ) -> anyhow::Result<Vec<MetricSnapshot>> {
        let mut families = crate::proxy::metrics::METRICS_REGISTRY.gather();
        families.extend(crate::apis::metrics::CONTROLLER_METRICS_REGISTRY.gather());
        families.extend(crate::proxy::rate_limiter::rate_limiter_registry().gather());
        families.extend(crate::proxy::circuit_breaker::circuit_breaker_registry().gather());

        let mut snapshots: Vec<_> = families
            .iter()
            .filter(|family| {
                metric_filter
                    .map(|filter| family.name().contains(filter))
                    .unwrap_or(true)
            })
            .map(metric_family_to_snapshot)
            .collect();

        snapshots.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(snapshots)
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

fn metric_family_to_snapshot(family: &MetricFamily) -> MetricSnapshot {
    MetricSnapshot {
        name: family.name().to_string(),
        help: family.help().to_string(),
        metric_type: metric_type_name(family.get_field_type()).to_string(),
        values: family
            .get_metric()
            .iter()
            .map(|metric| metric_to_value(family.get_field_type(), metric))
            .collect(),
    }
}

fn metric_to_value(metric_type: MetricType, metric: &Metric) -> MetricValue {
    let labels = metric
        .get_label()
        .iter()
        .map(|label| (label.name().to_string(), label.value().to_string()))
        .collect::<HashMap<_, _>>();

    MetricValue {
        labels,
        value: match metric_type {
            MetricType::COUNTER => metric.get_counter().value(),
            MetricType::GAUGE => metric.get_gauge().value(),
            MetricType::UNTYPED => 0.0,
            MetricType::HISTOGRAM => metric.get_histogram().sample_count() as f64,
            MetricType::SUMMARY => metric.get_summary().sample_count() as f64,
        },
    }
}

fn metric_type_name(metric_type: MetricType) -> &'static str {
    match metric_type {
        MetricType::COUNTER => "counter",
        MetricType::GAUGE => "gauge",
        MetricType::SUMMARY => "summary",
        MetricType::UNTYPED => "untyped",
        MetricType::HISTOGRAM => "histogram",
    }
}

impl LocalGatewayQuery {
    fn cache_stats_internal(&self) -> CacheStats {
        let (hits, misses) = self.router.get_cache_stats();
        let size = self.router.get_cache_size();
        let total = hits + misses;
        let hit_rate = if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        };

        CacheStats {
            hits,
            misses,
            size,
            hit_rate,
        }
    }
}
