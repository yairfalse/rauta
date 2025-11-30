//! Circuit Breaker Pattern Implementation
//!
//! Production-grade circuit breaker for backend health management:
//! - Three states: Closed, Open, Half-Open
//! - Configurable failure threshold and timeout
//! - Automatic recovery testing
//! - Per-backend isolation
//! - Thread-safe atomic operations
//!
//! Algorithm: https://martinfowler.com/bliki/CircuitBreaker.html
//!
//! Example:
//! ```rust,ignore
//! let breaker = CircuitBreaker::new(5, Duration::from_secs(30));
//!
//! // Record request result
//! breaker.record_success();
//! breaker.record_failure();
//!
//! // Check if requests allowed
//! if breaker.allow_request() {
//!     // Send request to backend
//! } else {
//!     // Backend is in Open state - fail fast
//! }
//! ```

use lazy_static::lazy_static;
use prometheus::{IntCounterVec, IntGaugeVec, Opts, Registry};
use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};
use tracing::warn;

lazy_static! {
    /// Global metrics registry for circuit breaker
    static ref CIRCUIT_BREAKER_REGISTRY: Registry = Registry::new();

    /// Circuit breaker state gauge (0=Closed, 1=Open, 2=HalfOpen)
    ///
    /// Labels:
    /// - backend: Backend identifier (e.g., "10.0.1.1:8080")
    ///
    /// State values:
    /// - 0 = Closed (healthy, requests allowed)
    /// - 1 = Open (unhealthy, requests rejected)
    /// - 2 = HalfOpen (testing recovery, limited requests allowed)
    ///
    /// Example PromQL queries:
    /// - Backends in Open state: `rauta_circuit_breaker_state{backend=~".*"} == 1`
    /// - Backends currently recovering: `rauta_circuit_breaker_state{backend=~".*"} == 2`
    #[allow(clippy::expect_used)]
    static ref CIRCUIT_BREAKER_STATE: IntGaugeVec = {
        let opts = Opts::new(
            "rauta_circuit_breaker_state",
            "Current state of circuit breaker (0=Closed, 1=Open, 2=HalfOpen)"
        );
        let gauge = IntGaugeVec::new(opts, &["backend"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create rauta_circuit_breaker_state gauge: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntGaugeVec::new(
                        Opts::new("rauta_circuit_breaker_state_fallback", "Fallback metric for circuit breaker state"),
                        &["backend"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = CIRCUIT_BREAKER_REGISTRY.register(Box::new(gauge.clone())) {
            eprintln!("WARN: Failed to register rauta_circuit_breaker_state gauge: {}", e);
        }
        gauge
    };

    /// Circuit breaker requests counter (allowed vs rejected)
    ///
    /// Labels:
    /// - backend: Backend identifier (e.g., "10.0.1.1:8080")
    /// - result: "allowed" or "rejected"
    ///
    /// Example PromQL queries:
    /// - Rate of rejected requests: `rate(rauta_circuit_breaker_requests_total{result="rejected"}[1m])`
    /// - Rejection ratio: `rate(rauta_circuit_breaker_requests_total{result="rejected"}[1m]) / rate(rauta_circuit_breaker_requests_total[1m])`
    #[allow(clippy::expect_used)]
    static ref CIRCUIT_BREAKER_REQUESTS_TOTAL: IntCounterVec = {
        let opts = Opts::new(
            "rauta_circuit_breaker_requests_total",
            "Total number of requests processed by circuit breaker (allowed or rejected)"
        );
        let counter = IntCounterVec::new(opts, &["backend", "result"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create rauta_circuit_breaker_requests_total counter: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntCounterVec::new(
                        Opts::new("rauta_circuit_breaker_requests_total_fallback", "Fallback metric for circuit breaker requests"),
                        &["backend", "result"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = CIRCUIT_BREAKER_REGISTRY.register(Box::new(counter.clone())) {
            eprintln!("WARN: Failed to register rauta_circuit_breaker_requests_total counter: {}", e);
        }
        counter
    };

    /// Circuit breaker failures counter
    ///
    /// Labels:
    /// - backend: Backend identifier (e.g., "10.0.1.1:8080")
    ///
    /// Tracks consecutive failures that lead to circuit opening.
    ///
    /// Example PromQL queries:
    /// - Failure rate by backend: `rate(rauta_circuit_breaker_failures_total[1m])`
    /// - Backends with high failure rates: `rate(rauta_circuit_breaker_failures_total[5m]) > 0.5`
    #[allow(clippy::expect_used)]
    static ref CIRCUIT_BREAKER_FAILURES_TOTAL: IntCounterVec = {
        let opts = Opts::new(
            "rauta_circuit_breaker_failures_total",
            "Total number of failures recorded by circuit breaker"
        );
        let counter = IntCounterVec::new(opts, &["backend"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create rauta_circuit_breaker_failures_total counter: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntCounterVec::new(
                        Opts::new("rauta_circuit_breaker_failures_total_fallback", "Fallback metric for circuit breaker failures"),
                        &["backend"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = CIRCUIT_BREAKER_REGISTRY.register(Box::new(counter.clone())) {
            eprintln!("WARN: Failed to register rauta_circuit_breaker_failures_total counter: {}", e);
        }
        counter
    };

    /// Circuit breaker state transitions counter
    ///
    /// Labels:
    /// - backend: Backend identifier (e.g., "10.0.1.1:8080")
    /// - from_state: Previous state (Closed/Open/HalfOpen)
    /// - to_state: New state (Closed/Open/HalfOpen)
    ///
    /// Tracks circuit breaker state changes for debugging and alerting.
    ///
    /// Example PromQL queries:
    /// - Circuits opening: `rate(rauta_circuit_breaker_transitions_total{to_state="Open"}[5m])`
    /// - Recovery attempts: `rauta_circuit_breaker_transitions_total{from_state="Open",to_state="HalfOpen"}`
    #[allow(clippy::expect_used)]
    static ref CIRCUIT_BREAKER_TRANSITIONS_TOTAL: IntCounterVec = {
        let opts = Opts::new(
            "rauta_circuit_breaker_transitions_total",
            "Total number of circuit breaker state transitions"
        );
        let counter = IntCounterVec::new(opts, &["backend", "from_state", "to_state"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create rauta_circuit_breaker_transitions_total counter: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntCounterVec::new(
                        Opts::new("rauta_circuit_breaker_transitions_total_fallback", "Fallback metric for circuit breaker transitions"),
                        &["backend", "from_state", "to_state"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = CIRCUIT_BREAKER_REGISTRY.register(Box::new(counter.clone())) {
            eprintln!("WARN: Failed to register rauta_circuit_breaker_transitions_total counter: {}", e);
        }
        counter
    };
}

/// Export the circuit breaker metrics registry (for global /metrics endpoint)
pub fn circuit_breaker_registry() -> &'static Registry {
    &CIRCUIT_BREAKER_REGISTRY
}

/// Safe RwLock read helper that recovers from poisoning
#[inline]
fn safe_read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| {
        warn!("RwLock poisoned during read, recovering (data is still valid)");
        poisoned.into_inner()
    })
}

/// Safe RwLock write helper that recovers from poisoning
#[inline]
fn safe_write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|poisoned| {
        warn!("RwLock poisoned during write, recovering (data is still valid)");
        poisoned.into_inner()
    })
}

/// Convert CircuitState to string for metrics labels
#[inline]
fn state_to_str(state: CircuitState) -> &'static str {
    match state {
        CircuitState::Closed => "Closed",
        CircuitState::Open => "Open",
        CircuitState::HalfOpen => "HalfOpen",
    }
}

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation - requests allowed, failures counted
    Closed,
    /// Too many failures - requests blocked, wait for timeout
    Open,
    /// Testing recovery - limited requests allowed
    HalfOpen,
}

/// Circuit breaker for backend health management
///
/// Implements the circuit breaker pattern with configurable thresholds.
/// Thread-safe via interior mutability.
#[derive(Debug)]
pub struct CircuitBreaker {
    /// Current state
    state: RwLock<CircuitState>,
    /// Failure threshold (consecutive failures to trip)
    failure_threshold: u32,
    /// Success count (in Half-Open state)
    success_count: RwLock<u32>,
    /// Failure count (consecutive)
    failure_count: RwLock<u32>,
    /// Timeout before attempting Half-Open (Open → Half-Open)
    timeout: Duration,
    /// Last failure timestamp
    last_failure_time: RwLock<Option<Instant>>,
    /// Half-open test request count
    half_open_requests: RwLock<u32>,
    /// Max requests in Half-Open state
    half_open_max_requests: u32,
}

impl CircuitBreaker {
    /// Create a new circuit breaker
    ///
    /// # Arguments
    /// * `failure_threshold` - Consecutive failures before opening circuit
    /// * `timeout` - Duration to wait before attempting Half-Open
    ///
    /// # Example
    /// ```rust,ignore
    /// let breaker = CircuitBreaker::new(5, Duration::from_secs(30));
    /// ```
    pub fn new(failure_threshold: u32, timeout: Duration) -> Self {
        Self {
            state: RwLock::new(CircuitState::Closed),
            failure_threshold,
            success_count: RwLock::new(0),
            failure_count: RwLock::new(0),
            timeout,
            last_failure_time: RwLock::new(None),
            half_open_requests: RwLock::new(0),
            half_open_max_requests: 3, // Allow 3 test requests in Half-Open
        }
    }

    /// Check if request should be allowed
    ///
    /// Returns true if request allowed, false if circuit is Open
    pub fn allow_request(&self) -> bool {
        // Check if Open state should transition to Half-Open
        {
            let state = *safe_read(&self.state);
            if state == CircuitState::Open && self.should_attempt_reset() {
                // Transition to Half-Open
                *safe_write(&self.state) = CircuitState::HalfOpen;
                *safe_write(&self.half_open_requests) = 0;
            }
        }

        // Now check current state and allow/deny request
        let state = *safe_read(&self.state);
        match state {
            CircuitState::Closed => true,
            CircuitState::Open => false,
            CircuitState::HalfOpen => {
                // Allow limited requests for testing
                let mut requests = safe_write(&self.half_open_requests);
                if *requests < self.half_open_max_requests {
                    *requests += 1;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record successful request
    pub fn record_success(&self) {
        let state = *safe_read(&self.state);

        match state {
            CircuitState::Closed => {
                // Reset failure count on success
                *safe_write(&self.failure_count) = 0;
            }
            CircuitState::HalfOpen => {
                // Increment success count and check threshold
                let should_close = {
                    let mut success_count = safe_write(&self.success_count);
                    *success_count += 1;
                    *success_count >= self.half_open_max_requests
                };

                // If enough successes, close the circuit
                if should_close {
                    *safe_write(&self.state) = CircuitState::Closed;
                    *safe_write(&self.failure_count) = 0;
                    *safe_write(&self.success_count) = 0;
                    *safe_write(&self.last_failure_time) = None;
                }
            }
            CircuitState::Open => {
                // Ignore successes in Open state (shouldn't happen)
            }
        }
    }

    /// Record failed request
    pub fn record_failure(&self) {
        let state = *safe_read(&self.state);

        match state {
            CircuitState::Closed => {
                // Increment failure count and check threshold
                let (should_open, now) = {
                    let mut failure_count = safe_write(&self.failure_count);
                    *failure_count += 1;
                    (*failure_count >= self.failure_threshold, Instant::now())
                };

                // Update last failure time
                *safe_write(&self.last_failure_time) = Some(now);

                // Open circuit if threshold exceeded
                if should_open {
                    *safe_write(&self.state) = CircuitState::Open;
                }
            }
            CircuitState::HalfOpen => {
                // Any failure in Half-Open immediately reopens circuit
                *safe_write(&self.state) = CircuitState::Open;
                *safe_write(&self.success_count) = 0;
                *safe_write(&self.half_open_requests) = 0;
                *safe_write(&self.last_failure_time) = Some(Instant::now());
            }
            CircuitState::Open => {
                // Update last failure time
                *safe_write(&self.last_failure_time) = Some(Instant::now());
            }
        }
    }

    /// Get current circuit state
    pub fn state(&self) -> CircuitState {
        *safe_read(&self.state)
    }

    /// Get current failure count
    #[allow(dead_code)] // Part of public API, used in tests
    pub fn failure_count(&self) -> u32 {
        *safe_read(&self.failure_count)
    }

    /// Check if circuit should attempt reset (Open → Half-Open)
    fn should_attempt_reset(&self) -> bool {
        if let Some(last_failure) = *safe_read(&self.last_failure_time) {
            last_failure.elapsed() >= self.timeout
        } else {
            false
        }
    }

    /// Reset circuit to Closed state (for testing)
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn reset(&self) {
        *safe_write(&self.state) = CircuitState::Closed;
        *safe_write(&self.failure_count) = 0;
        *safe_write(&self.success_count) = 0;
        *safe_write(&self.last_failure_time) = None;
        *safe_write(&self.half_open_requests) = 0;
    }
}

/// Per-backend circuit breaker manager
///
/// Manages circuit breakers for multiple backends with automatic cleanup.
pub struct CircuitBreakerManager {
    /// Backend ID -> CircuitBreaker mapping
    breakers: Arc<RwLock<HashMap<String, Arc<CircuitBreaker>>>>,
    /// Default failure threshold
    default_failure_threshold: u32,
    /// Default timeout
    default_timeout: Duration,
}

impl CircuitBreakerManager {
    /// Create a new circuit breaker manager
    ///
    /// # Arguments
    /// * `failure_threshold` - Default consecutive failures before opening
    /// * `timeout` - Default duration before attempting Half-Open
    pub fn new(failure_threshold: u32, timeout: Duration) -> Self {
        Self {
            breakers: Arc::new(RwLock::new(HashMap::new())),
            default_failure_threshold: failure_threshold,
            default_timeout: timeout,
        }
    }

    /// Get or create circuit breaker for backend
    pub fn get_breaker(&self, backend_id: &str) -> Arc<CircuitBreaker> {
        let breakers = safe_read(&self.breakers);

        if let Some(breaker) = breakers.get(backend_id) {
            Arc::clone(breaker)
        } else {
            // Release read lock before acquiring write lock
            drop(breakers);

            // Create new breaker
            let breaker = Arc::new(CircuitBreaker::new(
                self.default_failure_threshold,
                self.default_timeout,
            ));

            let mut breakers = safe_write(&self.breakers);
            breakers.insert(backend_id.to_string(), Arc::clone(&breaker));

            breaker
        }
    }

    /// Check if backend allows requests
    pub fn allow_request(&self, backend_id: &str) -> bool {
        let breaker = self.get_breaker(backend_id);
        let old_state = breaker.state();
        let allowed = breaker.allow_request();
        let new_state = breaker.state();

        // Record metrics
        let result = if allowed { "allowed" } else { "rejected" };
        CIRCUIT_BREAKER_REQUESTS_TOTAL
            .with_label_values(&[backend_id, result])
            .inc();

        // Update state gauge
        let state_value = match new_state {
            CircuitState::Closed => 0,
            CircuitState::Open => 1,
            CircuitState::HalfOpen => 2,
        };
        CIRCUIT_BREAKER_STATE
            .with_label_values(&[backend_id])
            .set(state_value);

        // Record state transition if changed
        if old_state != new_state {
            CIRCUIT_BREAKER_TRANSITIONS_TOTAL
                .with_label_values(&[backend_id, state_to_str(old_state), state_to_str(new_state)])
                .inc();
        }

        allowed
    }

    /// Record successful request
    pub fn record_success(&self, backend_id: &str) {
        let breaker = self.get_breaker(backend_id);
        let old_state = breaker.state();
        breaker.record_success();
        let new_state = breaker.state();

        // Update state gauge
        let state_value = match new_state {
            CircuitState::Closed => 0,
            CircuitState::Open => 1,
            CircuitState::HalfOpen => 2,
        };
        CIRCUIT_BREAKER_STATE
            .with_label_values(&[backend_id])
            .set(state_value);

        // Record state transition if changed
        if old_state != new_state {
            CIRCUIT_BREAKER_TRANSITIONS_TOTAL
                .with_label_values(&[backend_id, state_to_str(old_state), state_to_str(new_state)])
                .inc();
        }
    }

    /// Record failed request
    pub fn record_failure(&self, backend_id: &str) {
        let breaker = self.get_breaker(backend_id);
        let old_state = breaker.state();
        breaker.record_failure();
        let new_state = breaker.state();

        // Increment failure counter
        CIRCUIT_BREAKER_FAILURES_TOTAL
            .with_label_values(&[backend_id])
            .inc();

        // Update state gauge
        let state_value = match new_state {
            CircuitState::Closed => 0,
            CircuitState::Open => 1,
            CircuitState::HalfOpen => 2,
        };
        CIRCUIT_BREAKER_STATE
            .with_label_values(&[backend_id])
            .set(state_value);

        // Record state transition if changed
        if old_state != new_state {
            CIRCUIT_BREAKER_TRANSITIONS_TOTAL
                .with_label_values(&[backend_id, state_to_str(old_state), state_to_str(new_state)])
                .inc();
        }
    }

    /// Get backend circuit state
    #[allow(dead_code)] // Part of public API, used in tests
    pub fn get_state(&self, backend_id: &str) -> Option<CircuitState> {
        let breakers = safe_read(&self.breakers);
        breakers.get(backend_id).map(|b| b.state())
    }

    /// Remove circuit breaker for backend
    #[allow(dead_code)] // Part of public API, may be used for cleanup
    pub fn remove_backend(&self, backend_id: &str) {
        let mut breakers = safe_write(&self.breakers);
        breakers.remove(backend_id);
    }
}

impl Default for CircuitBreakerManager {
    fn default() -> Self {
        Self::new(5, Duration::from_secs(30))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_circuit_breaker_starts_closed() {
        let breaker = CircuitBreaker::new(5, Duration::from_secs(30));

        assert_eq!(breaker.state(), CircuitState::Closed);
        assert!(breaker.allow_request());
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let breaker = CircuitBreaker::new(3, Duration::from_secs(30));

        // Record 3 failures (threshold)
        for i in 1..=3 {
            breaker.record_failure();

            if i < 3 {
                assert_eq!(
                    breaker.state(),
                    CircuitState::Closed,
                    "Circuit should stay Closed until threshold"
                );
            } else {
                assert_eq!(
                    breaker.state(),
                    CircuitState::Open,
                    "Circuit should Open at threshold"
                );
            }
        }

        // Requests should be blocked
        assert!(
            !breaker.allow_request(),
            "Requests should be blocked in Open state"
        );
    }

    #[test]
    fn test_circuit_breaker_resets_failure_count_on_success() {
        let breaker = CircuitBreaker::new(5, Duration::from_secs(30));

        // Record 2 failures
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.failure_count(), 2);

        // Success resets count
        breaker.record_success();
        assert_eq!(breaker.failure_count(), 0);
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_half_open_after_timeout() {
        let breaker = CircuitBreaker::new(3, Duration::from_millis(100));

        // Open the circuit
        for _ in 0..3 {
            breaker.record_failure();
        }
        assert_eq!(breaker.state(), CircuitState::Open);

        // Wait for timeout
        thread::sleep(Duration::from_millis(150));

        // First request should transition to Half-Open
        assert!(
            breaker.allow_request(),
            "First request after timeout should be allowed (Half-Open)"
        );
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_circuit_breaker_half_open_closes_on_success() {
        let breaker = CircuitBreaker::new(3, Duration::from_millis(100));

        // Open the circuit
        for _ in 0..3 {
            breaker.record_failure();
        }

        // Wait for timeout and transition to Half-Open
        thread::sleep(Duration::from_millis(150));
        breaker.allow_request();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // Record 3 successful test requests (half_open_max_requests = 3)
        breaker.record_success();
        breaker.record_success();
        breaker.record_success();

        // Circuit should close
        assert_eq!(
            breaker.state(),
            CircuitState::Closed,
            "Circuit should close after successful Half-Open tests"
        );
        assert_eq!(breaker.failure_count(), 0);
    }

    #[test]
    fn test_circuit_breaker_half_open_reopens_on_failure() {
        let breaker = CircuitBreaker::new(3, Duration::from_millis(100));

        // Open the circuit
        for _ in 0..3 {
            breaker.record_failure();
        }

        // Wait for timeout and transition to Half-Open
        thread::sleep(Duration::from_millis(150));
        breaker.allow_request();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // Any failure in Half-Open reopens circuit
        breaker.record_failure();
        assert_eq!(
            breaker.state(),
            CircuitState::Open,
            "Circuit should reopen on Half-Open failure"
        );
    }

    #[test]
    fn test_circuit_breaker_half_open_limited_requests() {
        let breaker = CircuitBreaker::new(3, Duration::from_millis(100));

        // Open the circuit
        for _ in 0..3 {
            breaker.record_failure();
        }

        // Wait for timeout
        thread::sleep(Duration::from_millis(150));

        // Half-Open allows limited requests (max 3)
        assert!(breaker.allow_request(), "Request 1 should be allowed");
        assert!(breaker.allow_request(), "Request 2 should be allowed");
        assert!(breaker.allow_request(), "Request 3 should be allowed");
        assert!(
            !breaker.allow_request(),
            "Request 4 should be blocked (limit reached)"
        );
    }

    #[test]
    fn test_circuit_breaker_manager_per_backend_isolation() {
        let manager = CircuitBreakerManager::new(3, Duration::from_secs(30));

        // Create backend-2 first (simulate a successful request)
        manager.record_success("backend-2");

        // Fail backend-1
        for _ in 0..3 {
            manager.record_failure("backend-1");
        }

        // backend-1 should be Open
        assert_eq!(
            manager.get_state("backend-1"),
            Some(CircuitState::Open),
            "backend-1 should be Open"
        );

        // backend-2 should still be Closed (isolated from backend-1)
        assert_eq!(
            manager.get_state("backend-2"),
            Some(CircuitState::Closed),
            "backend-2 should be Closed (isolated from backend-1)"
        );
        assert!(
            manager.allow_request("backend-2"),
            "backend-2 should allow requests"
        );
    }

    #[test]
    fn test_circuit_breaker_manager_create_on_demand() {
        let manager = CircuitBreakerManager::new(3, Duration::from_secs(30));

        // get_state on nonexistent backend returns None
        assert_eq!(manager.get_state("new-backend"), None);

        // get_breaker creates a new breaker
        let breaker = manager.get_breaker("new-backend");
        assert_eq!(breaker.state(), CircuitState::Closed);

        // Second access reuses breaker
        manager.record_failure("new-backend");
        assert_eq!(manager.get_breaker("new-backend").failure_count(), 1);
    }

    #[test]
    fn test_circuit_breaker_manager_remove_backend() {
        let manager = CircuitBreakerManager::new(3, Duration::from_secs(30));

        // Create breaker for backend
        manager.record_failure("backend-1");
        assert_eq!(manager.get_state("backend-1"), Some(CircuitState::Closed));
        assert_eq!(manager.get_breaker("backend-1").failure_count(), 1);

        // Remove backend
        manager.remove_backend("backend-1");
        assert_eq!(
            manager.get_state("backend-1"),
            None,
            "get_state should return None after removal"
        );

        // get_breaker creates a fresh breaker
        let new_breaker = manager.get_breaker("backend-1");
        assert_eq!(new_breaker.state(), CircuitState::Closed);
        assert_eq!(
            new_breaker.failure_count(),
            0,
            "New breaker should have zero failures"
        );
    }

    #[test]
    fn test_circuit_breaker_concurrent_access() {
        use std::sync::Arc;

        let breaker = Arc::new(CircuitBreaker::new(10, Duration::from_secs(30)));

        // Spawn 5 threads, each recording 2 failures
        let handles: Vec<_> = (0..5)
            .map(|_| {
                let breaker = Arc::clone(&breaker);
                thread::spawn(move || {
                    breaker.record_failure();
                    breaker.record_failure();
                })
            })
            .collect();

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Total 10 failures (exactly at threshold)
        assert_eq!(
            breaker.state(),
            CircuitState::Open,
            "Circuit should Open after 10 concurrent failures"
        );
    }

    #[test]
    fn test_circuit_breaker_metrics_recorded() {
        let manager = CircuitBreakerManager::new(3, Duration::from_secs(30));

        // Allow request (should be allowed initially)
        assert!(manager.allow_request("backend1"));

        // Record some failures to trigger state change
        manager.record_failure("backend1");
        manager.record_failure("backend1");
        manager.record_failure("backend1");

        // Circuit should now be Open
        assert_eq!(manager.get_state("backend1"), Some(CircuitState::Open));

        // Check metrics are accessible
        let metrics = crate::proxy::circuit_breaker::circuit_breaker_registry().gather();

        // Should have rauta_circuit_breaker_state metric
        let has_state_metric = metrics
            .iter()
            .any(|family| family.name() == "rauta_circuit_breaker_state");
        assert!(
            has_state_metric,
            "Should have rauta_circuit_breaker_state metric"
        );

        // Should have rauta_circuit_breaker_requests_total metric
        let has_requests_metric = metrics
            .iter()
            .any(|family| family.name() == "rauta_circuit_breaker_requests_total");
        assert!(
            has_requests_metric,
            "Should have rauta_circuit_breaker_requests_total metric"
        );

        // Should have rauta_circuit_breaker_failures_total metric
        let has_failures_metric = metrics
            .iter()
            .any(|family| family.name() == "rauta_circuit_breaker_failures_total");
        assert!(
            has_failures_metric,
            "Should have rauta_circuit_breaker_failures_total metric"
        );

        // Should have rauta_circuit_breaker_transitions_total metric
        let has_transitions_metric = metrics
            .iter()
            .any(|family| family.name() == "rauta_circuit_breaker_transitions_total");
        assert!(
            has_transitions_metric,
            "Should have rauta_circuit_breaker_transitions_total metric"
        );
    }

    #[test]
    fn test_circuit_breaker_state_transitions_full_cycle() {
        let breaker = CircuitBreaker::new(2, Duration::from_millis(100));

        // 1. Start Closed
        assert_eq!(breaker.state(), CircuitState::Closed);

        // 2. Fail twice → Open
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);

        // 3. Wait for timeout → Half-Open
        thread::sleep(Duration::from_millis(150));
        breaker.allow_request();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // 4. Succeed 3 times → Closed
        breaker.record_success();
        breaker.record_success();
        breaker.record_success();
        assert_eq!(breaker.state(), CircuitState::Closed);

        // 5. Verify failure count reset
        assert_eq!(breaker.failure_count(), 0);
    }
}
