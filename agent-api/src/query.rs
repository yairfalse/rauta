//! Gateway Query Trait
//!
//! Defines the abstract interface for querying gateway state.
//! Two implementations:
//! 1. `LocalGatewayQuery` (in control crate) — reads from `Arc<Router>` directly
//! 2. `RemoteGatewayQuery` (in rauta-cli crate) — HTTP/Unix socket client

use crate::actions::ActionResult;
use crate::ebpf::TcpHealthEvidenceSnapshot;
use crate::temporal::{GatewayDiff, TemporalQuery, TimelineSnapshot};
use crate::types::{
    CacheStats, CircuitBreakerSnapshot, Diagnosis, GatewaySnapshot, ListenerSnapshot,
    MetricSnapshot, RateLimiterSnapshot, RouteSnapshot,
};
use async_trait::async_trait;

/// Abstract interface for querying gateway state
///
/// All methods are read-only except bounded safe-action methods.
#[async_trait]
pub trait GatewayQuery: Send + Sync {
    /// Get gateway status overview
    async fn snapshot(&self) -> anyhow::Result<GatewaySnapshot>;

    /// List all configured routes
    async fn list_routes(
        &self,
        method_filter: Option<&str>,
        path_prefix: Option<&str>,
    ) -> anyhow::Result<Vec<RouteSnapshot>>;

    /// Get a single route by pattern
    async fn get_route(&self, pattern: &str) -> anyhow::Result<Option<RouteSnapshot>>;

    /// List circuit breaker states
    async fn list_circuit_breakers(
        &self,
        state_filter: Option<&str>,
    ) -> anyhow::Result<Vec<CircuitBreakerSnapshot>>;

    /// List rate limiter states
    async fn list_rate_limiters(
        &self,
        route_filter: Option<&str>,
    ) -> anyhow::Result<Vec<RateLimiterSnapshot>>;

    /// List active listeners
    async fn list_listeners(&self) -> anyhow::Result<Vec<ListenerSnapshot>>;

    /// Get route cache statistics
    async fn cache_stats(&self) -> anyhow::Result<Option<CacheStats>>;

    /// Get metrics snapshot
    async fn metrics_snapshot(
        &self,
        metric_filter: Option<&str>,
    ) -> anyhow::Result<Vec<MetricSnapshot>>;

    /// Optional TCP health evidence from userspace/mock/eBPF sensors
    async fn tcp_health_evidence(&self) -> anyhow::Result<TcpHealthEvidenceSnapshot>;

    /// Read recent bounded temporal history
    async fn timeline(&self, query: TemporalQuery) -> anyhow::Result<TimelineSnapshot>;

    /// Diff recent gateway state over a bounded time window
    async fn diff(&self, query: TemporalQuery) -> anyhow::Result<GatewayDiff>;

    /// Run diagnostics for a symptom
    async fn diagnose(
        &self,
        symptom: &str,
        route_filter: Option<&str>,
        backend_filter: Option<&str>,
    ) -> anyhow::Result<Vec<Diagnosis>>;

    /// Run diagnostics using recent temporal evidence when available.
    async fn diagnose_since(
        &self,
        symptom: &str,
        route_filter: Option<&str>,
        backend_filter: Option<&str>,
        _since_seconds: Option<u64>,
    ) -> anyhow::Result<Vec<Diagnosis>> {
        self.diagnose(symptom, route_filter, backend_filter).await
    }

    /// Drain a backend (graceful removal)
    async fn drain_backend(
        &self,
        backend: &str,
        timeout_secs: Option<u64>,
    ) -> anyhow::Result<ActionResult>;

    /// Cancel drain for a backend
    async fn undrain_backend(&self, backend: &str) -> anyhow::Result<ActionResult>;

    /// Quarantine a backend until expiry
    async fn quarantine_backend(
        &self,
        backend: &str,
        ttl_secs: u64,
    ) -> anyhow::Result<ActionResult>;
}
