//! Local Gateway Query Implementation
//!
//! Reads directly from `Arc<Router>`, `Arc<CircuitBreakerManager>`, and `Arc<RateLimiter>`.
//! Used by the admin server and MCP server when running in-process.

use agent_api::actions::{
    ActionEvidence, ActionPrecondition, ActionResult, ActionRisk, ActionStatus, RollbackMetadata,
};
use agent_api::ontology::{ActionKind, EntityKind, EntityRef, EvidenceValue};
use agent_api::query::GatewayQuery;
use agent_api::temporal::{
    GatewayDiff, SnapshotHistoryEntry, TemporalEvent, TemporalEventKind, TemporalQuery,
    TimelineSnapshot, DEFAULT_TEMPORAL_RETENTION,
};
use agent_api::types::*;
use async_trait::async_trait;
use common::Backend;
use prometheus::proto::{Metric, MetricFamily, MetricType};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::observability::ebpf::{sensor_from_env, TcpHealthSensor};
use crate::proxy::circuit_breaker::CircuitBreakerManager;
use crate::proxy::rate_limiter::RateLimiter;
use crate::proxy::router::Router;

/// Local query implementation that reads from shared gateway state
pub struct LocalGatewayQuery {
    router: Arc<Router>,
    circuit_breaker: Arc<CircuitBreakerManager>,
    rate_limiter: Arc<RateLimiter>,
    start_time: Instant,
    temporal: Mutex<TemporalState>,
    tcp_sensor: Box<dyn TcpHealthSensor>,
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
            temporal: Mutex::new(TemporalState::new(DEFAULT_TEMPORAL_RETENTION)),
            tcp_sensor: sensor_from_env(),
        }
    }
}

#[derive(Clone)]
struct ObservedState {
    entry: SnapshotHistoryEntry,
    routes: Vec<RouteSnapshot>,
    circuit_breakers: Vec<CircuitBreakerSnapshot>,
    rate_limiters: Vec<RateLimiterSnapshot>,
    listeners: Vec<ListenerSnapshot>,
}

struct TemporalState {
    retention: usize,
    next_sequence: u64,
    events: VecDeque<TemporalEvent>,
    observations: VecDeque<ObservedState>,
}

struct BackendActionBuild {
    kind: ActionKind,
    backend: Backend,
    risk: ActionRisk,
    ttl_secs: u64,
    before: ActionEvidence,
    after: ActionEvidence,
    expires_at_unix_ms: Option<u64>,
}

impl TemporalState {
    fn new(retention: usize) -> Self {
        Self {
            retention,
            next_sequence: 1,
            events: VecDeque::with_capacity(retention),
            observations: VecDeque::with_capacity(retention),
        }
    }

    fn push_observation(&mut self, mut observation: ObservedState) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        observation.entry.sequence = sequence;

        if let Some(previous) = self.observations.back().cloned() {
            self.push_diff_events(sequence, &observation, &previous);
        } else {
            self.push_event(TemporalEvent {
                sequence,
                timestamp_unix_ms: observation.entry.timestamp_unix_ms,
                kind: TemporalEventKind::SnapshotRecorded,
                subject: EntityRef::new(EntityKind::Gateway, "rauta"),
                summary: "Initial temporal gateway snapshot recorded".to_string(),
                attributes: BTreeMap::new(),
            });
        }

        self.observations.push_back(observation);
        while self.observations.len() > self.retention {
            self.observations.pop_front();
        }
    }

    fn push_diff_events(
        &mut self,
        sequence: u64,
        current: &ObservedState,
        previous: &ObservedState,
    ) {
        let timestamp_unix_ms = current.entry.timestamp_unix_ms;
        if current.entry.snapshot.route_count != previous.entry.snapshot.route_count {
            self.push_event(counter_event(
                sequence,
                timestamp_unix_ms,
                TemporalEventKind::RouteChanged,
                EntityRef::new(EntityKind::Route, "routes"),
                "Route count changed",
                "route_count",
                current.entry.snapshot.route_count,
            ));
        }
        if current.entry.snapshot.open_circuits != previous.entry.snapshot.open_circuits {
            self.push_event(counter_event(
                sequence,
                timestamp_unix_ms,
                TemporalEventKind::CircuitBreakerChanged,
                EntityRef::new(EntityKind::CircuitBreaker, "all"),
                "Open circuit count changed",
                "open_circuits",
                current.entry.snapshot.open_circuits,
            ));
        }
        if current.entry.snapshot.exhausted_rate_limiters
            != previous.entry.snapshot.exhausted_rate_limiters
        {
            self.push_event(counter_event(
                sequence,
                timestamp_unix_ms,
                TemporalEventKind::RateLimiterChanged,
                EntityRef::new(EntityKind::RateLimiter, "all"),
                "Exhausted rate limiter count changed",
                "exhausted_rate_limiters",
                current.entry.snapshot.exhausted_rate_limiters,
            ));
        }

        for route in changed_keys(
            route_signatures(&previous.routes),
            route_signatures(&current.routes),
        ) {
            self.push_event(simple_event(
                sequence,
                timestamp_unix_ms,
                TemporalEventKind::RouteChanged,
                EntityKind::Route,
                route,
                "Route semantics changed",
            ));
        }
        for backend in changed_keys(
            breaker_signatures(&previous.circuit_breakers),
            breaker_signatures(&current.circuit_breakers),
        ) {
            self.push_event(simple_event(
                sequence,
                timestamp_unix_ms,
                TemporalEventKind::CircuitBreakerChanged,
                EntityKind::CircuitBreaker,
                backend,
                "Circuit breaker state changed",
            ));
        }
        for route in changed_keys(
            limiter_signatures(&previous.rate_limiters),
            limiter_signatures(&current.rate_limiters),
        ) {
            self.push_event(simple_event(
                sequence,
                timestamp_unix_ms,
                TemporalEventKind::RateLimiterChanged,
                EntityKind::RateLimiter,
                route,
                "Rate limiter state changed",
            ));
        }
        for listener in changed_keys(
            listener_signatures(&previous.listeners),
            listener_signatures(&current.listeners),
        ) {
            self.push_event(simple_event(
                sequence,
                timestamp_unix_ms,
                TemporalEventKind::ListenerChanged,
                EntityKind::Listener,
                listener,
                "Listener state changed",
            ));
        }
    }

    fn push_event(&mut self, event: TemporalEvent) {
        self.events.push_back(event);
        while self.events.len() > self.retention {
            self.events.pop_front();
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

        let snapshot = GatewaySnapshot {
            status: "ok".to_string(),
            uptime_seconds: uptime,
            route_count,
            open_circuits,
            exhausted_rate_limiters,
            listeners: vec![],
            cache_stats: Some(self.cache_stats_internal()),
        };
        self.record_observation(snapshot.clone())?;
        Ok(snapshot)
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

    async fn tcp_health_evidence(
        &self,
    ) -> anyhow::Result<agent_api::ebpf::TcpHealthEvidenceSnapshot> {
        Ok(self.tcp_sensor.snapshot())
    }

    async fn timeline(&self, query: TemporalQuery) -> anyhow::Result<TimelineSnapshot> {
        self.snapshot().await?;

        let temporal = self
            .temporal
            .lock()
            .map_err(|_| anyhow::anyhow!("temporal state lock poisoned"))?;
        let cutoff_ms = query
            .since_seconds
            .map(|seconds| now_unix_ms().saturating_sub(seconds.saturating_mul(1000)));
        let events = temporal
            .events
            .iter()
            .filter(|event| temporal_event_matches(event, &query, cutoff_ms))
            .cloned()
            .collect();
        let snapshots = temporal
            .observations
            .iter()
            .filter(|observation| {
                cutoff_ms
                    .map(|cutoff| observation.entry.timestamp_unix_ms >= cutoff)
                    .unwrap_or(true)
                    && query
                        .from_sequence
                        .map(|from| observation.entry.sequence >= from)
                        .unwrap_or(true)
                    && query
                        .to_sequence
                        .map(|to| observation.entry.sequence <= to)
                        .unwrap_or(true)
            })
            .map(|observation| observation.entry.clone())
            .collect();

        Ok(TimelineSnapshot {
            retention: temporal.retention,
            generated_at_unix_ms: now_unix_ms(),
            events,
            snapshots,
        })
    }

    async fn diff(&self, query: TemporalQuery) -> anyhow::Result<GatewayDiff> {
        self.snapshot().await?;
        let temporal = self
            .temporal
            .lock()
            .map_err(|_| anyhow::anyhow!("temporal state lock poisoned"))?;
        Ok(build_diff(&temporal, &query))
    }

    async fn diagnose(
        &self,
        symptom: &str,
        route_filter: Option<&str>,
        backend_filter: Option<&str>,
    ) -> anyhow::Result<Vec<Diagnosis>> {
        use agent_api::diagnostics::engine::{DiagnosticContext, DiagnosticsEngine};

        let mut snapshot = self.snapshot().await?;
        let mut routes = self.router.list_routes();
        apply_route_filter(&mut routes, route_filter);
        apply_backend_filter_to_routes(&mut routes, backend_filter);
        let mut circuit_breakers = self.circuit_breaker.snapshot_all();
        apply_backend_filter_to_breakers(&mut circuit_breakers, backend_filter);
        let mut rate_limiters = self.rate_limiter.snapshot_all();
        apply_route_filter_to_limiters(&mut rate_limiters, route_filter);
        let mut tcp_health = self.tcp_sensor.snapshot();
        apply_backend_filter_to_tcp_health(&mut tcp_health, backend_filter);

        snapshot.route_count = routes.len();
        snapshot.open_circuits = circuit_breakers
            .iter()
            .filter(|breaker| breaker.state == "Open")
            .count();
        snapshot.exhausted_rate_limiters = rate_limiters
            .iter()
            .filter(|limiter| limiter.tokens_available <= 0.0)
            .count();

        let ctx = DiagnosticContext {
            snapshot,
            routes,
            circuit_breakers,
            rate_limiters,
            tcp_health: Some(tcp_health),
        };

        let engine = DiagnosticsEngine::with_builtin_rules();
        Ok(engine.diagnose_symptom(&ctx, symptom))
    }

    async fn diagnose_since(
        &self,
        symptom: &str,
        route_filter: Option<&str>,
        backend_filter: Option<&str>,
        since_seconds: Option<u64>,
    ) -> anyhow::Result<Vec<Diagnosis>> {
        let mut diagnoses = self.diagnose(symptom, route_filter, backend_filter).await?;
        if let Some(seconds) = since_seconds {
            let diff = self
                .diff(TemporalQuery {
                    since_seconds: Some(seconds),
                    ..TemporalQuery::default()
                })
                .await?;
            if !diff.events.is_empty() {
                let evidence = format!(
                    "{} temporal events observed in the last {}s",
                    diff.events.len(),
                    seconds
                );
                for diagnosis in &mut diagnoses {
                    diagnosis.evidence.push(evidence.clone());
                }
            }
        }
        Ok(diagnoses)
    }

    async fn drain_backend(
        &self,
        backend: &str,
        timeout_secs: Option<u64>,
    ) -> anyhow::Result<ActionResult> {
        self.apply_backend_action(
            ActionKind::DrainBackend,
            backend,
            timeout_secs.unwrap_or(30),
            ActionRisk::Medium,
        )
    }

    async fn undrain_backend(&self, backend: &str) -> anyhow::Result<ActionResult> {
        self.apply_undrain_action(backend)
    }

    async fn quarantine_backend(
        &self,
        backend: &str,
        ttl_secs: u64,
    ) -> anyhow::Result<ActionResult> {
        self.apply_backend_action(
            ActionKind::QuarantineBackend,
            backend,
            ttl_secs,
            ActionRisk::High,
        )
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

    fn record_observation(&self, snapshot: GatewaySnapshot) -> anyhow::Result<()> {
        let mut routes = self.router.list_routes();
        routes.sort_by(|a, b| a.pattern.cmp(&b.pattern));
        let mut circuit_breakers = self.circuit_breaker.snapshot_all();
        circuit_breakers.sort_by(|a, b| a.backend_id.cmp(&b.backend_id));
        let mut rate_limiters = self.rate_limiter.snapshot_all();
        rate_limiters.sort_by(|a, b| a.route.cmp(&b.route));
        let listeners = snapshot.listeners.clone();

        let observation = ObservedState {
            entry: SnapshotHistoryEntry {
                sequence: 0,
                timestamp_unix_ms: now_unix_ms(),
                snapshot,
            },
            routes,
            circuit_breakers,
            rate_limiters,
            listeners,
        };

        self.temporal
            .lock()
            .map_err(|_| anyhow::anyhow!("temporal state lock poisoned"))?
            .push_observation(observation);
        Ok(())
    }

    fn apply_backend_action(
        &self,
        kind: ActionKind,
        backend_text: &str,
        ttl_secs: u64,
        risk: ActionRisk,
    ) -> anyhow::Result<ActionResult> {
        if ttl_secs == 0 || ttl_secs > 86_400 {
            anyhow::bail!("action_failed: ttl_secs must be between 1 and 86400");
        }
        let backend = parse_backend(backend_text)?;
        let before = self.action_evidence(backend);
        let exists = !before.affected_routes.is_empty();
        if !exists {
            anyhow::bail!(
                "action_failed: backend {} is not present in any route",
                backend
            );
        }
        if before.is_draining {
            anyhow::bail!("action_failed: backend {} is already draining", backend);
        }

        self.router
            .drain_backend(backend, Duration::from_secs(ttl_secs));
        let after = self.action_evidence(backend);
        let expires_at_unix_ms = Some(now_unix_ms().saturating_add(ttl_secs.saturating_mul(1000)));
        self.action_result(BackendActionBuild {
            kind,
            backend,
            risk,
            ttl_secs,
            before,
            after,
            expires_at_unix_ms,
        })
    }

    fn apply_undrain_action(&self, backend_text: &str) -> anyhow::Result<ActionResult> {
        let backend = parse_backend(backend_text)?;
        let before = self.action_evidence(backend);
        if before.affected_routes.is_empty() {
            anyhow::bail!(
                "action_failed: backend {} is not present in any route",
                backend
            );
        }
        if !before.is_draining {
            anyhow::bail!("action_failed: backend {} is not draining", backend);
        }

        self.router.undrain_backend(backend);
        let after = self.action_evidence(backend);
        self.action_result(BackendActionBuild {
            kind: ActionKind::UndrainBackend,
            backend,
            risk: ActionRisk::Low,
            ttl_secs: 0,
            before,
            after,
            expires_at_unix_ms: None,
        })
    }

    fn action_result(&self, build: BackendActionBuild) -> anyhow::Result<ActionResult> {
        let sequence = self.next_temporal_sequence()?;
        let summary = match build.kind {
            ActionKind::DrainBackend => format!("Backend {} marked draining", build.backend),
            ActionKind::UndrainBackend => {
                format!("Backend {} restored to active service", build.backend)
            }
            ActionKind::QuarantineBackend => format!("Backend {} quarantined", build.backend),
            _ => format!("Backend action applied to {}", build.backend),
        };
        let mut attributes = BTreeMap::new();
        attributes.insert("ttl_secs".to_string(), EvidenceValue::from(build.ttl_secs));
        attributes.insert(
            "affected_routes".to_string(),
            EvidenceValue::from(build.after.affected_routes.len()),
        );
        let timeline_event = TemporalEvent {
            sequence,
            timestamp_unix_ms: now_unix_ms(),
            kind: TemporalEventKind::AdminAction,
            subject: EntityRef::new(EntityKind::Backend, build.backend.to_string()),
            summary,
            attributes,
        };
        self.push_temporal_event(timeline_event.clone())?;

        let rollback = match build.kind {
            ActionKind::DrainBackend | ActionKind::QuarantineBackend => Some(RollbackMetadata {
                action: ActionKind::UndrainBackend,
                cli_command: format!("rauta backends undrain {}", build.backend),
                reason: "Restore backend to active routing before expiry".to_string(),
            }),
            ActionKind::UndrainBackend => Some(RollbackMetadata {
                action: ActionKind::DrainBackend,
                cli_command: format!("rauta backends drain {}", build.backend),
                reason: "Reapply draining if backend remains unsafe".to_string(),
            }),
            _ => None,
        };

        Ok(ActionResult {
            action_id: format!("action-{}-{}", timeline_event.sequence, build.backend),
            kind: build.kind,
            target: EntityRef::new(EntityKind::Backend, build.backend.to_string()),
            status: ActionStatus::Applied,
            risk: build.risk,
            preconditions: vec![
                ActionPrecondition {
                    name: "backend_present".to_string(),
                    passed: true,
                    message: "Backend is referenced by at least one route".to_string(),
                },
                ActionPrecondition {
                    name: "bounded_duration".to_string(),
                    passed: true,
                    message: "Action duration is bounded".to_string(),
                },
            ],
            before: build.before,
            after: build.after,
            rollback,
            expires_at_unix_ms: build.expires_at_unix_ms,
            timeline_event,
        })
    }

    fn action_evidence(&self, backend: Backend) -> ActionEvidence {
        let backend_text = backend.to_string();
        let affected_routes = self
            .router
            .list_routes()
            .into_iter()
            .filter(|route| {
                route.backends.iter().any(|candidate| {
                    format!("{}:{}", candidate.address, candidate.port) == backend_text
                })
            })
            .map(|route| route.pattern)
            .collect();
        let is_draining = self.router.is_backend_draining(backend);

        ActionEvidence {
            backend: backend_text,
            was_draining: is_draining,
            is_draining,
            affected_routes,
        }
    }

    fn next_temporal_sequence(&self) -> anyhow::Result<u64> {
        let mut temporal = self
            .temporal
            .lock()
            .map_err(|_| anyhow::anyhow!("temporal state lock poisoned"))?;
        let sequence = temporal.next_sequence;
        temporal.next_sequence += 1;
        Ok(sequence)
    }

    fn push_temporal_event(&self, event: TemporalEvent) -> anyhow::Result<()> {
        self.temporal
            .lock()
            .map_err(|_| anyhow::anyhow!("temporal state lock poisoned"))?
            .push_event(event);
        Ok(())
    }
}

fn parse_backend(value: &str) -> anyhow::Result<Backend> {
    let socket_addr: SocketAddr = value
        .parse()
        .map_err(|e| anyhow::anyhow!("action_failed: backend must be host:port: {}", e))?;
    Ok(match socket_addr {
        SocketAddr::V4(addr) => Backend::from_ipv4(*addr.ip(), addr.port(), 1),
        SocketAddr::V6(addr) => Backend::from_ipv6(*addr.ip(), addr.port(), 1),
    })
}

fn apply_route_filter(routes: &mut Vec<RouteSnapshot>, route_filter: Option<&str>) {
    if let Some(filter) = route_filter {
        routes.retain(|route| route.pattern.contains(filter));
    }
}

fn apply_backend_filter_to_routes(routes: &mut Vec<RouteSnapshot>, backend_filter: Option<&str>) {
    if let Some(filter) = backend_filter {
        routes.retain_mut(|route| {
            route
                .backends
                .retain(|backend| backend_id(backend).contains(filter));
            !route.backends.is_empty()
        });
    }
}

fn apply_backend_filter_to_breakers(
    breakers: &mut Vec<CircuitBreakerSnapshot>,
    backend_filter: Option<&str>,
) {
    if let Some(filter) = backend_filter {
        breakers.retain(|breaker| breaker.backend_id.contains(filter));
    }
}

fn apply_route_filter_to_limiters(
    limiters: &mut Vec<RateLimiterSnapshot>,
    route_filter: Option<&str>,
) {
    if let Some(filter) = route_filter {
        limiters.retain(|limiter| limiter.route.contains(filter));
    }
}

fn apply_backend_filter_to_tcp_health(
    tcp_health: &mut agent_api::ebpf::TcpHealthEvidenceSnapshot,
    backend_filter: Option<&str>,
) {
    if let Some(filter) = backend_filter {
        tcp_health
            .signals
            .retain(|signal| signal.backend.contains(filter));
    }
}

fn backend_id(backend: &BackendSnapshot) -> String {
    format!("{}:{}", backend.address, backend.port)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn temporal_event_matches(
    event: &TemporalEvent,
    query: &TemporalQuery,
    cutoff_ms: Option<u64>,
) -> bool {
    cutoff_ms
        .map(|cutoff| event.timestamp_unix_ms >= cutoff)
        .unwrap_or(true)
        && query
            .from_sequence
            .map(|from| event.sequence >= from)
            .unwrap_or(true)
        && query
            .to_sequence
            .map(|to| event.sequence <= to)
            .unwrap_or(true)
}

fn build_diff(temporal: &TemporalState, query: &TemporalQuery) -> GatewayDiff {
    let Some(current) = temporal.observations.back() else {
        return GatewayDiff {
            from_sequence: None,
            to_sequence: 0,
            since_seconds: query.since_seconds,
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
        };
    };

    let cutoff_ms = query
        .since_seconds
        .map(|seconds| now_unix_ms().saturating_sub(seconds.saturating_mul(1000)));
    let baseline = temporal
        .observations
        .iter()
        .find(|observation| {
            cutoff_ms
                .map(|cutoff| observation.entry.timestamp_unix_ms >= cutoff)
                .unwrap_or(true)
                && query
                    .from_sequence
                    .map(|from| observation.entry.sequence >= from)
                    .unwrap_or(true)
        })
        .unwrap_or(current);

    let previous_routes = route_signatures(&baseline.routes);
    let current_routes = route_signatures(&current.routes);
    let previous_listeners = listener_signatures(&baseline.listeners);
    let current_listeners = listener_signatures(&current.listeners);

    GatewayDiff {
        from_sequence: Some(baseline.entry.sequence),
        to_sequence: current.entry.sequence,
        since_seconds: query.since_seconds,
        route_count_delta: current.entry.snapshot.route_count as isize
            - baseline.entry.snapshot.route_count as isize,
        open_circuits_delta: current.entry.snapshot.open_circuits as isize
            - baseline.entry.snapshot.open_circuits as isize,
        exhausted_rate_limiters_delta: current.entry.snapshot.exhausted_rate_limiters as isize
            - baseline.entry.snapshot.exhausted_rate_limiters as isize,
        added_routes: added_keys(&previous_routes, &current_routes),
        removed_routes: removed_keys(&previous_routes, &current_routes),
        changed_routes: modified_keys(&previous_routes, &current_routes),
        added_listeners: added_keys(&previous_listeners, &current_listeners),
        removed_listeners: removed_keys(&previous_listeners, &current_listeners),
        changed_circuit_breakers: changed_keys(
            breaker_signatures(&baseline.circuit_breakers),
            breaker_signatures(&current.circuit_breakers),
        ),
        changed_rate_limiters: changed_keys(
            limiter_signatures(&baseline.rate_limiters),
            limiter_signatures(&current.rate_limiters),
        ),
        events: temporal
            .events
            .iter()
            .filter(|event| temporal_event_matches(event, query, cutoff_ms))
            .cloned()
            .collect(),
    }
}

fn route_signatures(routes: &[RouteSnapshot]) -> BTreeMap<String, String> {
    routes
        .iter()
        .map(|route| {
            (
                route.pattern.clone(),
                serde_json::to_string(route).unwrap_or_else(|_| route.pattern.clone()),
            )
        })
        .collect()
}

fn listener_signatures(listeners: &[ListenerSnapshot]) -> BTreeMap<String, String> {
    listeners
        .iter()
        .map(|listener| {
            let id = format!("{}/{}", listener.protocol, listener.port);
            (
                id.clone(),
                serde_json::to_string(listener).unwrap_or_else(|_| id.clone()),
            )
        })
        .collect()
}

fn breaker_signatures(breakers: &[CircuitBreakerSnapshot]) -> BTreeMap<String, String> {
    breakers
        .iter()
        .map(|breaker| {
            (
                breaker.backend_id.clone(),
                serde_json::to_string(breaker).unwrap_or_else(|_| breaker.backend_id.clone()),
            )
        })
        .collect()
}

fn limiter_signatures(limiters: &[RateLimiterSnapshot]) -> BTreeMap<String, String> {
    limiters
        .iter()
        .map(|limiter| {
            (
                limiter.route.clone(),
                serde_json::to_string(limiter).unwrap_or_else(|_| limiter.route.clone()),
            )
        })
        .collect()
}

fn added_keys(
    previous: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> Vec<String> {
    current
        .keys()
        .filter(|key| !previous.contains_key(*key))
        .cloned()
        .collect()
}

fn removed_keys(
    previous: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> Vec<String> {
    previous
        .keys()
        .filter(|key| !current.contains_key(*key))
        .cloned()
        .collect()
}

fn modified_keys(
    previous: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> Vec<String> {
    current
        .iter()
        .filter(|(key, value)| {
            previous
                .get(*key)
                .is_some_and(|previous_value| previous_value != *value)
        })
        .map(|(key, _)| key.clone())
        .collect()
}

fn changed_keys(
    previous: BTreeMap<String, String>,
    current: BTreeMap<String, String>,
) -> Vec<String> {
    let keys = previous
        .keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter(|key| previous.get(key) != current.get(key))
        .collect()
}

fn counter_event(
    sequence: u64,
    timestamp_unix_ms: u64,
    kind: TemporalEventKind,
    subject: EntityRef,
    summary: &str,
    attribute: &str,
    value: usize,
) -> TemporalEvent {
    let mut attributes = BTreeMap::new();
    attributes.insert(attribute.to_string(), EvidenceValue::from(value));
    TemporalEvent {
        sequence,
        timestamp_unix_ms,
        kind,
        subject,
        summary: summary.to_string(),
        attributes,
    }
}

fn simple_event(
    sequence: u64,
    timestamp_unix_ms: u64,
    kind: TemporalEventKind,
    entity_kind: EntityKind,
    id: String,
    summary: &str,
) -> TemporalEvent {
    TemporalEvent {
        sequence,
        timestamp_unix_ms,
        kind,
        subject: EntityRef::new(entity_kind, id),
        summary: summary.to_string(),
        attributes: BTreeMap::new(),
    }
}
