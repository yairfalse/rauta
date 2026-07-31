//! Remote Gateway Query
//!
//! HTTP client that talks to the RAUTA admin server REST API.
//! Implements `GatewayQuery` trait so it can be used with the MCP handler.

use agent_api::actions::ActionResult;
use agent_api::query::GatewayQuery;
use agent_api::temporal::{GatewayDiff, TemporalQuery, TimelineSnapshot};
use agent_api::types::*;
use async_trait::async_trait;

pub struct RemoteGatewayQuery {
    base_url: String,
    client: reqwest::Client,
}

impl RemoteGatewayQuery {
    pub fn new(endpoint: &str) -> Self {
        Self {
            base_url: endpoint.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    // Convenience methods for direct CLI use (without going through the trait)

    pub async fn get_status(&self) -> anyhow::Result<GatewaySnapshot> {
        self.snapshot().await
    }

    pub async fn get_routes(
        &self,
        method_filter: Option<&str>,
    ) -> anyhow::Result<Vec<RouteSnapshot>> {
        self.list_routes(method_filter, None).await
    }

    pub async fn get_route(&self, pattern: &str) -> anyhow::Result<Option<RouteSnapshot>> {
        GatewayQuery::get_route(self, pattern).await
    }

    pub async fn diagnose_since(
        &self,
        symptom: &str,
        since_seconds: Option<u64>,
    ) -> anyhow::Result<Vec<Diagnosis>> {
        GatewayQuery::diagnose_since(self, symptom, None, None, since_seconds).await
    }

    pub async fn drain_backend(
        &self,
        backend: &str,
        timeout_secs: u64,
    ) -> anyhow::Result<ActionResult> {
        GatewayQuery::drain_backend(self, backend, Some(timeout_secs)).await
    }

    pub async fn undrain_backend(&self, backend: &str) -> anyhow::Result<ActionResult> {
        GatewayQuery::undrain_backend(self, backend).await
    }

    pub async fn quarantine_backend(
        &self,
        backend: &str,
        ttl_secs: u64,
    ) -> anyhow::Result<ActionResult> {
        GatewayQuery::quarantine_backend(self, backend, ttl_secs).await
    }
}

#[async_trait]
impl GatewayQuery for RemoteGatewayQuery {
    async fn snapshot(&self) -> anyhow::Result<GatewaySnapshot> {
        let url = format!("{}/api/v1/status", self.base_url);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn list_routes(
        &self,
        method_filter: Option<&str>,
        path_prefix: Option<&str>,
    ) -> anyhow::Result<Vec<RouteSnapshot>> {
        let url = format!("{}/api/v1/routes", self.base_url);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let mut routes: Vec<RouteSnapshot> = resp.json().await?;

        // Apply filters client-side (admin API returns all routes)
        if let Some(method) = method_filter {
            let m = method.to_uppercase();
            routes.retain(|r| r.method == m);
        }
        if let Some(prefix) = path_prefix {
            routes.retain(|r| r.pattern.starts_with(prefix));
        }

        Ok(routes)
    }

    async fn get_route(&self, pattern: &str) -> anyhow::Result<Option<RouteSnapshot>> {
        let routes = self.list_routes(None, None).await?;
        Ok(routes.into_iter().find(|r| r.pattern == pattern))
    }

    async fn list_circuit_breakers(
        &self,
        state_filter: Option<&str>,
    ) -> anyhow::Result<Vec<CircuitBreakerSnapshot>> {
        let url = format!("{}/api/v1/circuit-breakers", self.base_url);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let mut breakers: Vec<CircuitBreakerSnapshot> = resp.json().await?;

        if let Some(state) = state_filter {
            let normalized = state.to_uppercase();
            breakers.retain(|b| b.state.to_uppercase() == normalized);
        }

        Ok(breakers)
    }

    async fn list_rate_limiters(
        &self,
        route_filter: Option<&str>,
    ) -> anyhow::Result<Vec<RateLimiterSnapshot>> {
        let url = format!("{}/api/v1/rate-limiters", self.base_url);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let mut limiters: Vec<RateLimiterSnapshot> = resp.json().await?;

        if let Some(route) = route_filter {
            limiters.retain(|l| l.route.contains(route));
        }

        Ok(limiters)
    }

    async fn list_listeners(&self) -> anyhow::Result<Vec<ListenerSnapshot>> {
        let url = format!("{}/api/v1/listeners", self.base_url);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn cache_stats(&self) -> anyhow::Result<Option<CacheStats>> {
        let url = format!("{}/api/v1/cache", self.base_url);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn metrics_snapshot(
        &self,
        metric_filter: Option<&str>,
    ) -> anyhow::Result<Vec<MetricSnapshot>> {
        let url = format!("{}/api/v1/metrics", self.base_url);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let mut metrics: Vec<MetricSnapshot> = resp.json().await?;

        if let Some(metric) = metric_filter {
            metrics.retain(|m| m.name.contains(metric));
        }

        Ok(metrics)
    }

    async fn timeline(&self, query: TemporalQuery) -> anyhow::Result<TimelineSnapshot> {
        let url = format!("{}/api/v1/timeline", self.base_url);
        let resp = self
            .client
            .get(&url)
            .query(&temporal_query_pairs(&query))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn diff(&self, query: TemporalQuery) -> anyhow::Result<GatewayDiff> {
        let url = format!("{}/api/v1/diff", self.base_url);
        let resp = self
            .client
            .get(&url)
            .query(&temporal_query_pairs(&query))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn diagnose(
        &self,
        symptom: &str,
        _route_filter: Option<&str>,
        _backend_filter: Option<&str>,
    ) -> anyhow::Result<Vec<Diagnosis>> {
        let url = format!("{}/api/v1/diagnose", self.base_url);
        let resp = self
            .client
            .post(&url)
            .query(&[("symptom", symptom)])
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn diagnose_since(
        &self,
        symptom: &str,
        _route_filter: Option<&str>,
        _backend_filter: Option<&str>,
        since_seconds: Option<u64>,
    ) -> anyhow::Result<Vec<Diagnosis>> {
        let url = format!("{}/api/v1/diagnose", self.base_url);
        let mut query = vec![("symptom".to_string(), symptom.to_string())];
        if let Some(seconds) = since_seconds {
            query.push(("since_seconds".to_string(), seconds.to_string()));
        }
        let resp = self
            .client
            .post(&url)
            .query(&query)
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn drain_backend(
        &self,
        backend: &str,
        timeout_secs: Option<u64>,
    ) -> anyhow::Result<ActionResult> {
        let url = format!("{}/api/v1/backends/drain", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "backend": backend,
                "timeout_secs": timeout_secs.unwrap_or(30)
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn undrain_backend(&self, backend: &str) -> anyhow::Result<ActionResult> {
        let url = format!("{}/api/v1/backends/undrain", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "backend": backend }))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn quarantine_backend(
        &self,
        backend: &str,
        ttl_secs: u64,
    ) -> anyhow::Result<ActionResult> {
        let url = format!("{}/api/v1/backends/quarantine", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "backend": backend,
                "ttl_secs": ttl_secs
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }
}

fn temporal_query_pairs(query: &TemporalQuery) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    if let Some(value) = query.since_seconds {
        pairs.push(("since_seconds".to_string(), value.to_string()));
    }
    if let Some(value) = query.from_sequence {
        pairs.push(("from_sequence".to_string(), value.to_string()));
    }
    if let Some(value) = query.to_sequence {
        pairs.push(("to_sequence".to_string(), value.to_string()));
    }
    pairs
}
