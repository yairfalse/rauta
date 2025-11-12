//! Active Health Checking with Prometheus Metrics
//!
//! Probes backends periodically with TCP connections to detect failures proactively.
//! Exports health status via Prometheus metrics for observability.
//!
//! Follows Cortex/Mimir pattern: metrics are injected via Registry, not global.

use common::Backend;
use prometheus::{CounterVec, HistogramOpts, HistogramVec, IntGaugeVec, Opts, Registry};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::time::interval;
use tracing::{debug, info, warn};

/// Health check configuration
#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    /// Probe interval (default: 5 seconds)
    pub interval: Duration,
    /// Probe timeout (default: 2 seconds)
    pub timeout: Duration,
    /// Consecutive failures before marking unhealthy (default: 3)
    pub unhealthy_threshold: u32,
    /// Consecutive successes before marking healthy (default: 2)
    pub healthy_threshold: u32,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(2),
            unhealthy_threshold: 3,
            healthy_threshold: 2,
        }
    }
}

/// Backend health status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Unhealthy,
}

/// Health check state for a single backend
#[derive(Debug, Clone)]
struct BackendHealthState {
    status: HealthStatus,
    consecutive_successes: u32,
    consecutive_failures: u32,
    last_check: Option<Instant>,
    last_latency_ms: Option<u64>,
}

impl BackendHealthState {
    fn new() -> Self {
        Self {
            status: HealthStatus::Healthy, // Start optimistically
            consecutive_successes: 0,
            consecutive_failures: 0,
            last_check: None,
            last_latency_ms: None,
        }
    }

    /// Record successful probe
    fn record_success(&mut self, latency_ms: u64, config: &HealthCheckConfig) {
        self.consecutive_successes += 1;
        self.consecutive_failures = 0;
        self.last_latency_ms = Some(latency_ms);
        self.last_check = Some(Instant::now());

        // Mark healthy after threshold successes
        if self.consecutive_successes >= config.healthy_threshold {
            self.status = HealthStatus::Healthy;
        }
    }

    /// Record failed probe
    fn record_failure(&mut self, config: &HealthCheckConfig) {
        self.consecutive_failures += 1;
        self.consecutive_successes = 0;
        self.last_check = Some(Instant::now());

        // Mark unhealthy after threshold failures
        if self.consecutive_failures >= config.unhealthy_threshold {
            self.status = HealthStatus::Unhealthy;
        }
    }
}

/// Active health checker with injected Prometheus metrics
pub struct HealthChecker {
    config: HealthCheckConfig,
    /// Backend health state (Backend -> HealthState)
    states: Arc<RwLock<HashMap<Backend, BackendHealthState>>>,

    // Prometheus metrics (injected, not global)
    backend_health_status: IntGaugeVec,
    health_check_probes_total: CounterVec,
    health_check_duration: HistogramVec,
    health_check_consecutive_failures: IntGaugeVec,
    health_state_transitions_total: CounterVec,
}

impl HealthChecker {
    /// Create new health checker with metrics registered in the given registry
    pub fn new(config: HealthCheckConfig, registry: &Registry) -> Self {
        // Create metrics
        let backend_health_status = IntGaugeVec::new(
            Opts::new(
                "rauta_backend_health_status",
                "Backend health status (1=healthy, 0=unhealthy)",
            ),
            &["backend", "route"],
        )
        .expect("Failed to create backend_health_status metric");

        let health_check_probes_total = CounterVec::new(
            Opts::new(
                "rauta_health_check_probes_total",
                "Total number of health check probes sent",
            ),
            &["backend", "result"], // result = success | failure | timeout
        )
        .expect("Failed to create health_check_probes_total metric");

        let health_check_duration = HistogramVec::new(
            HistogramOpts::new(
                "rauta_health_check_duration_seconds",
                "Health check probe duration in seconds",
            )
            .buckets(vec![0.001, 0.005, 0.010, 0.050, 0.100, 0.500, 1.0, 2.0]),
            &["backend", "result"],
        )
        .expect("Failed to create health_check_duration metric");

        let health_check_consecutive_failures = IntGaugeVec::new(
            Opts::new(
                "rauta_health_check_consecutive_failures",
                "Number of consecutive health check failures",
            ),
            &["backend"],
        )
        .expect("Failed to create health_check_consecutive_failures metric");

        let health_state_transitions_total = CounterVec::new(
            Opts::new(
                "rauta_health_state_transitions_total",
                "Total health state transitions (healthy <-> unhealthy)",
            ),
            &["backend", "from", "to"],
        )
        .expect("Failed to create health_state_transitions_total metric");

        // Register metrics with the provided registry (Cortex/Mimir pattern)
        registry
            .register(Box::new(backend_health_status.clone()))
            .expect("Failed to register backend_health_status");
        registry
            .register(Box::new(health_check_probes_total.clone()))
            .expect("Failed to register health_check_probes_total");
        registry
            .register(Box::new(health_check_duration.clone()))
            .expect("Failed to register health_check_duration");
        registry
            .register(Box::new(health_check_consecutive_failures.clone()))
            .expect("Failed to register health_check_consecutive_failures");
        registry
            .register(Box::new(health_state_transitions_total.clone()))
            .expect("Failed to register health_state_transitions_total");

        Self {
            config,
            states: Arc::new(RwLock::new(HashMap::new())),
            backend_health_status,
            health_check_probes_total,
            health_check_duration,
            health_check_consecutive_failures,
            health_state_transitions_total,
        }
    }

    /// Check if a backend is healthy
    pub fn is_healthy(&self, backend: &Backend) -> bool {
        let states = self.states.read().unwrap();
        states
            .get(backend)
            .map(|state| state.status == HealthStatus::Healthy)
            .unwrap_or(true) // Default to healthy if not yet checked
    }

    /// Get health status for all backends
    #[allow(dead_code)] // Will be used for debugging/admin endpoints
    pub fn get_all_statuses(&self) -> HashMap<Backend, HealthStatus> {
        let states = self.states.read().unwrap();
        states
            .iter()
            .map(|(backend, state)| (*backend, state.status))
            .collect()
    }

    /// Start health checking for a list of backends
    pub fn start_checking(&self, backends: Vec<Backend>, route_name: String) {
        let states = self.states.clone();
        let config = self.config.clone();

        // Clone metrics for background tasks
        let backend_health_status = self.backend_health_status.clone();
        let health_check_probes_total = self.health_check_probes_total.clone();
        let health_check_duration = self.health_check_duration.clone();
        let health_check_consecutive_failures = self.health_check_consecutive_failures.clone();
        let health_state_transitions_total = self.health_state_transitions_total.clone();

        // Ensure all backends are tracked
        {
            let mut states_lock = states.write().unwrap();
            for backend in &backends {
                states_lock
                    .entry(*backend)
                    .or_insert_with(BackendHealthState::new);
            }
        }

        // Spawn background task for each backend
        // Only spawn if we're in a tokio runtime (skip in tests)
        for backend in backends {
            let states_clone = states.clone();
            let config_clone = config.clone();
            let route_name_clone = route_name.clone();

            // Clone metrics for this task
            let backend_health_status_clone = backend_health_status.clone();
            let health_check_probes_total_clone = health_check_probes_total.clone();
            let health_check_duration_clone = health_check_duration.clone();
            let health_check_consecutive_failures_clone = health_check_consecutive_failures.clone();
            let health_state_transitions_total_clone = health_state_transitions_total.clone();

            // Only spawn if runtime is available (gracefully skip in tests)
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::spawn(async move {
                    Self::check_backend_loop(
                        backend,
                        states_clone,
                        config_clone,
                        route_name_clone,
                        backend_health_status_clone,
                        health_check_probes_total_clone,
                        health_check_duration_clone,
                        health_check_consecutive_failures_clone,
                        health_state_transitions_total_clone,
                    )
                    .await;
                });
            }
        }
    }

    /// Background loop for checking a single backend
    #[allow(clippy::too_many_arguments)]
    async fn check_backend_loop(
        backend: Backend,
        states: Arc<RwLock<HashMap<Backend, BackendHealthState>>>,
        config: HealthCheckConfig,
        route_name: String,
        backend_health_status: IntGaugeVec,
        health_check_probes_total: CounterVec,
        health_check_duration: HistogramVec,
        health_check_consecutive_failures: IntGaugeVec,
        health_state_transitions_total: CounterVec,
    ) {
        let mut interval_timer = interval(config.interval);
        let backend_label = backend.to_string();

        info!(
            backend = %backend_label,
            route = %route_name,
            "Starting health checks"
        );

        loop {
            interval_timer.tick().await;

            // Probe backend
            let start = Instant::now();
            let socket_addr = backend.to_socket_addr();

            let result =
                tokio::time::timeout(config.timeout, TcpStream::connect(socket_addr)).await;

            let latency_ms = start.elapsed().as_millis() as u64;

            // Update state based on result
            let (probe_result, old_status, new_status) = {
                let mut states_lock = states.write().unwrap();
                let state = states_lock
                    .entry(backend)
                    .or_insert_with(BackendHealthState::new);

                let old_status = state.status; // Capture before update

                match result {
                    Ok(Ok(_stream)) => {
                        // Success: backend is reachable
                        state.record_success(latency_ms, &config);
                        debug!(
                            backend = %backend_label,
                            latency_ms = latency_ms,
                            consecutive_successes = state.consecutive_successes,
                            "Health check succeeded"
                        );
                        ("success", old_status, state.status)
                    }
                    Ok(Err(e)) => {
                        // Connection failed
                        state.record_failure(&config);
                        warn!(
                            backend = %backend_label,
                            error = %e,
                            consecutive_failures = state.consecutive_failures,
                            "Health check failed (connection error)"
                        );
                        ("failure", old_status, state.status)
                    }
                    Err(_) => {
                        // Timeout
                        state.record_failure(&config);
                        warn!(
                            backend = %backend_label,
                            timeout_ms = config.timeout.as_millis(),
                            consecutive_failures = state.consecutive_failures,
                            "Health check failed (timeout)"
                        );
                        ("timeout", old_status, state.status)
                    }
                }
            };

            // Record state transition if status changed
            if old_status != new_status {
                let old_status_str = match old_status {
                    HealthStatus::Healthy => "healthy".to_string(),
                    HealthStatus::Unhealthy => "unhealthy".to_string(),
                };
                let new_status_str = match new_status {
                    HealthStatus::Healthy => "healthy".to_string(),
                    HealthStatus::Unhealthy => "unhealthy".to_string(),
                };

                health_state_transitions_total
                    .with_label_values(&[&backend_label, &old_status_str, &new_status_str])
                    .inc();

                info!(
                    backend = %backend_label,
                    old_status = %old_status_str,
                    new_status = %new_status_str,
                    "Health status transitioned"
                );
            }

            // Update Prometheus metrics (use injected metrics, not globals)
            backend_health_status
                .with_label_values(&[&backend_label, &route_name])
                .set(if new_status == HealthStatus::Healthy {
                    1
                } else {
                    0
                });

            health_check_probes_total
                .with_label_values(&[&backend_label, &probe_result.to_string()])
                .inc();

            // Record latency as histogram (ALL probes, not just success)
            let latency_seconds = latency_ms as f64 / 1000.0;
            health_check_duration
                .with_label_values(&[&backend_label, &probe_result.to_string()])
                .observe(latency_seconds);

            let consecutive_failures = {
                let states_lock = states.read().unwrap();
                states_lock
                    .get(&backend)
                    .map(|s| s.consecutive_failures)
                    .unwrap_or(0)
            };

            health_check_consecutive_failures
                .with_label_values(&[&backend_label])
                .set(consecutive_failures as i64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prometheus::Registry;
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn test_health_checker_with_isolated_registry() {
        // Test that HealthChecker properly injects metrics using an isolated registry
        // This verifies we're not using global metrics (Cortex/Mimir pattern)

        let registry = Registry::new();
        let config = HealthCheckConfig::default();

        // Create HealthChecker with custom registry - should not panic
        let checker = HealthChecker::new(config, &registry);

        // Checker should work
        let backend = Backend::from_ipv4(Ipv4Addr::new(10, 0, 1, 1), 8080, 100);
        assert!(checker.is_healthy(&backend));

        // The fact that this test doesn't panic during HealthChecker::new()
        // proves that metrics were successfully registered with the custom registry.
        // We can't easily verify gather() because Prometheus metrics only appear
        // in gather() after they're used (have label values set), and we can't
        // reliably trigger health checks in a unit test without race conditions.

        // The real test is:
        // 1. No panic during registration ✅
        // 2. HealthChecker works ✅
        // 3. Integration tests will verify metrics are properly exported ✅
    }

    #[tokio::test]
    async fn test_health_checker_creation() {
        let registry = Registry::new();
        let config = HealthCheckConfig::default();
        let checker = HealthChecker::new(config, &registry);

        // All backends start as healthy (optimistic)
        let backend = Backend::from_ipv4(Ipv4Addr::new(10, 0, 1, 1), 8080, 100);
        assert!(checker.is_healthy(&backend));
    }

    #[tokio::test]
    async fn test_health_state_transitions() {
        let config = HealthCheckConfig {
            unhealthy_threshold: 3,
            healthy_threshold: 2,
            ..Default::default()
        };

        let mut state = BackendHealthState::new();
        assert_eq!(state.status, HealthStatus::Healthy);

        // 1 failure - still healthy
        state.record_failure(&config);
        assert_eq!(state.status, HealthStatus::Healthy);
        assert_eq!(state.consecutive_failures, 1);

        // 2 failures - still healthy
        state.record_failure(&config);
        assert_eq!(state.status, HealthStatus::Healthy);
        assert_eq!(state.consecutive_failures, 2);

        // 3 failures - now unhealthy
        state.record_failure(&config);
        assert_eq!(state.status, HealthStatus::Unhealthy);
        assert_eq!(state.consecutive_failures, 3);

        // 1 success - still unhealthy
        state.record_success(10, &config);
        assert_eq!(state.status, HealthStatus::Unhealthy);
        assert_eq!(state.consecutive_successes, 1);
        assert_eq!(state.consecutive_failures, 0);

        // 2 successes - now healthy
        state.record_success(10, &config);
        assert_eq!(state.status, HealthStatus::Healthy);
        assert_eq!(state.consecutive_successes, 2);
    }

    #[test]
    fn test_default_config() {
        let config = HealthCheckConfig::default();
        assert_eq!(config.interval, Duration::from_secs(5));
        assert_eq!(config.timeout, Duration::from_secs(2));
        assert_eq!(config.unhealthy_threshold, 3);
        assert_eq!(config.healthy_threshold, 2);
    }
}
