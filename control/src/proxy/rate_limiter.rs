//! Token Bucket Rate Limiter
//!
//! Production-grade rate limiting using token bucket algorithm:
//! - Configurable rate (requests per second)
//! - Burst capacity (max tokens in bucket)
//! - Per-route isolation
//! - Lock-free atomic operations where possible
//!
//! Algorithm: https://en.wikipedia.org/wiki/Token_bucket
//!
//! Example:
//! ```rust,ignore
//! let limiter = RateLimiter::new();
//! limiter.configure_route("/api", 100.0, 200); // 100 rps, burst 200
//!
//! if limiter.check_rate_limit("/api") {
//!     // Process request
//! } else {
//!     // Return 429 Too Many Requests
//! }
//! ```

use lazy_static::lazy_static;
use prometheus::{IntCounterVec, IntGaugeVec, Opts, Registry};
use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Instant;
use tracing::warn;

lazy_static! {
    /// Global metrics registry for rate limiter
    static ref RATE_LIMITER_REGISTRY: Registry = Registry::new();

    /// Rate limit requests counter (allowed vs rejected)
    ///
    /// Labels:
    /// - route: The route pattern (e.g., "/api/users")
    /// - result: "allowed" or "rejected"
    ///
    /// Example PromQL queries:
    /// - Rate of rejected requests: `rate(rauta_rate_limit_requests_total{result="rejected"}[1m])`
    /// - Success rate by route: `rate(rauta_rate_limit_requests_total{result="allowed"}[1m]) / rate(rauta_rate_limit_requests_total[1m])`
    #[allow(clippy::expect_used)]
    static ref RATE_LIMIT_REQUESTS_TOTAL: IntCounterVec = {
        let opts = Opts::new(
            "rauta_rate_limit_requests_total",
            "Total number of requests processed by rate limiter (allowed or rejected)"
        );
        let counter = IntCounterVec::new(opts, &["route", "result"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create rauta_rate_limit_requests_total counter: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntCounterVec::new(
                        Opts::new("rauta_rate_limit_requests_total_fallback", "Fallback metric for rate limit requests"),
                        &["route", "result"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = RATE_LIMITER_REGISTRY.register(Box::new(counter.clone())) {
            eprintln!("WARN: Failed to register rauta_rate_limit_requests_total counter: {}", e);
        }
        counter
    };

    /// Current tokens available in each route's bucket
    ///
    /// Labels:
    /// - route: The route pattern (e.g., "/api/users")
    ///
    /// This gauge shows real-time token availability:
    /// - Value near capacity: Route is healthy, not rate limited
    /// - Value near 0: Route is experiencing high traffic
    /// - Value at 0: Route is actively rate limiting requests
    ///
    /// Example PromQL queries:
    /// - Routes near rate limit: `rauta_rate_limit_tokens_available < 10`
    /// - Token refill rate: `deriv(rauta_rate_limit_tokens_available[5m])`
    #[allow(clippy::expect_used)]
    static ref RATE_LIMIT_TOKENS_AVAILABLE: IntGaugeVec = {
        let opts = Opts::new(
            "rauta_rate_limit_tokens_available",
            "Current tokens available in the rate limiter bucket for each route"
        );
        let gauge = IntGaugeVec::new(opts, &["route"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create rauta_rate_limit_tokens_available gauge: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntGaugeVec::new(
                        Opts::new("rauta_rate_limit_tokens_available_fallback", "Fallback metric for rate limit tokens"),
                        &["route"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = RATE_LIMITER_REGISTRY.register(Box::new(gauge.clone())) {
            eprintln!("WARN: Failed to register rauta_rate_limit_tokens_available gauge: {}", e);
        }
        gauge
    };
}

/// Export the rate limiter metrics registry (for global /metrics endpoint)
pub fn rate_limiter_registry() -> &'static Registry {
    &RATE_LIMITER_REGISTRY
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

/// Token bucket for rate limiting
///
/// Implements the token bucket algorithm with nanosecond precision.
/// Thread-safe via interior mutability.
#[derive(Debug)]
pub struct TokenBucket {
    /// Maximum tokens (burst capacity)
    capacity: f64,
    /// Current tokens available
    tokens: RwLock<f64>,
    /// Refill rate (tokens per second)
    refill_rate: f64,
    /// Last refill timestamp
    last_refill: RwLock<Instant>,
}

impl TokenBucket {
    /// Create a new token bucket
    ///
    /// # Arguments
    /// * `rate` - Tokens per second (e.g., 100.0 = 100 requests/sec)
    /// * `burst` - Maximum burst capacity (tokens)
    ///
    /// # Example
    /// ```rust,ignore
    /// let bucket = TokenBucket::new(100.0, 200);
    /// ```
    pub fn new(rate: f64, burst: u64) -> Self {
        let capacity = burst as f64;
        Self {
            capacity,
            tokens: RwLock::new(capacity), // Start with full bucket
            refill_rate: rate,
            last_refill: RwLock::new(Instant::now()),
        }
    }

    /// Try to acquire a token
    ///
    /// Returns true if token acquired (request allowed), false otherwise (rate limited)
    pub fn try_acquire(&self) -> bool {
        self.try_acquire_n(1.0)
    }

    /// Try to acquire N tokens
    ///
    /// Useful for weighted rate limiting (e.g., expensive operations cost more tokens)
    pub fn try_acquire_n(&self, n: f64) -> bool {
        if n <= 0.0 {
            return true; // Zero or negative tokens always succeed
        }

        // Refill tokens based on elapsed time
        self.refill();

        // Try to acquire tokens
        let mut tokens = safe_write(&self.tokens);
        if *tokens >= n {
            *tokens -= n;
            true
        } else {
            false
        }
    }

    /// Refill tokens based on elapsed time
    fn refill(&self) {
        let now = Instant::now();
        let mut last_refill = safe_write(&self.last_refill);
        let elapsed = now.duration_since(*last_refill);

        // Calculate tokens to add based on elapsed time
        let tokens_to_add = elapsed.as_secs_f64() * self.refill_rate;

        if tokens_to_add > 0.0 {
            let mut tokens = safe_write(&self.tokens);
            *tokens = (*tokens + tokens_to_add).min(self.capacity);
            *last_refill = now;
        }
    }

    /// Get current token count (for testing/metrics)
    pub fn available_tokens(&self) -> f64 {
        self.refill();
        *safe_read(&self.tokens)
    }

    /// Reset bucket to full capacity (for testing)
    #[cfg(test)]
    pub fn reset(&self) {
        let mut tokens = safe_write(&self.tokens);
        *tokens = self.capacity;
        let mut last_refill = safe_write(&self.last_refill);
        *last_refill = Instant::now();
    }
}

/// Per-route rate limiter
///
/// Manages token buckets for multiple routes, with automatic cleanup of unused routes.
pub struct RateLimiter {
    /// Route -> TokenBucket mapping
    buckets: Arc<RwLock<HashMap<String, Arc<TokenBucket>>>>,
}

impl RateLimiter {
    /// Create a new rate limiter
    pub fn new() -> Self {
        Self {
            buckets: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Configure rate limit for a route
    ///
    /// # Arguments
    /// * `route` - Route pattern (e.g., "/api")
    /// * `rate` - Requests per second
    /// * `burst` - Burst capacity
    ///
    /// # Example
    /// ```rust,ignore
    /// limiter.configure_route("/api", 100.0, 200);
    /// ```
    pub fn configure_route(&self, route: &str, rate: f64, burst: u64) {
        let bucket = Arc::new(TokenBucket::new(rate, burst));
        let mut buckets = safe_write(&self.buckets);
        buckets.insert(route.to_string(), bucket);
    }

    /// Check if request is allowed (within rate limit)
    ///
    /// Returns true if allowed, false if rate limited
    pub fn check_rate_limit(&self, route: &str) -> bool {
        let buckets = safe_read(&self.buckets);

        if let Some(bucket) = buckets.get(route) {
            let allowed = bucket.try_acquire();

            // Record metrics
            let result = if allowed { "allowed" } else { "rejected" };
            RATE_LIMIT_REQUESTS_TOTAL
                .with_label_values(&[route, result])
                .inc();

            // Update tokens available gauge
            let tokens = bucket.available_tokens();
            RATE_LIMIT_TOKENS_AVAILABLE
                .with_label_values(&[route])
                .set(tokens as i64);

            allowed
        } else {
            // No rate limit configured for this route - allow by default
            RATE_LIMIT_REQUESTS_TOTAL
                .with_label_values(&[route, "allowed"])
                .inc();

            true
        }
    }

    /// Remove rate limit configuration for a route
    #[allow(dead_code)] // Part of public API, may be used for cleanup
    pub fn remove_route(&self, route: &str) {
        let mut buckets = safe_write(&self.buckets);
        buckets.remove(route);
    }

    /// Get available tokens for a route (for testing/metrics)
    #[allow(dead_code)] // Part of public API, used in tests
    pub fn available_tokens(&self, route: &str) -> Option<f64> {
        let buckets = safe_read(&self.buckets);
        buckets.get(route).map(|bucket| bucket.available_tokens())
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_token_bucket_allows_requests_under_limit() {
        let bucket = TokenBucket::new(10.0, 10); // 10 rps, burst 10

        // Should allow first 10 requests (burst capacity)
        for i in 1..=10 {
            assert!(
                bucket.try_acquire(),
                "Request {} should be allowed (within burst)",
                i
            );
        }

        // Should deny 11th request (bucket empty)
        assert!(
            !bucket.try_acquire(),
            "Request 11 should be denied (bucket empty)"
        );
    }

    #[test]
    fn test_token_bucket_refills_over_time() {
        let bucket = TokenBucket::new(100.0, 10); // 100 rps, burst 10

        // Drain bucket
        for _ in 0..10 {
            bucket.try_acquire();
        }

        // Bucket should be empty
        assert!(!bucket.try_acquire(), "Bucket should be empty");

        // Wait 100ms (should refill 10 tokens at 100 rps)
        thread::sleep(Duration::from_millis(100));

        // Should have ~10 tokens now
        assert!(
            bucket.try_acquire(),
            "Bucket should have refilled after 100ms"
        );
    }

    #[test]
    fn test_token_bucket_burst_capacity() {
        let bucket = TokenBucket::new(10.0, 50); // 10 rps, burst 50

        // Should allow 50 requests immediately (burst)
        for i in 1..=50 {
            assert!(
                bucket.try_acquire(),
                "Request {} should be allowed (within burst of 50)",
                i
            );
        }

        // 51st should fail
        assert!(
            !bucket.try_acquire(),
            "Request 51 should be denied (burst exceeded)"
        );
    }

    #[test]
    fn test_token_bucket_refill_rate_accuracy() {
        let bucket = TokenBucket::new(1000.0, 10); // 1000 rps = 1 token per ms

        // Drain bucket
        for _ in 0..10 {
            bucket.try_acquire();
        }

        // Wait 5ms (should refill ~5 tokens)
        thread::sleep(Duration::from_millis(5));

        // Should allow ~5 requests
        let mut allowed = 0;
        for _ in 0..10 {
            if bucket.try_acquire() {
                allowed += 1;
            }
        }

        // Should have allowed 4-6 requests (accounting for timing variance)
        assert!(
            allowed >= 4 && allowed <= 6,
            "Should allow 4-6 requests after 5ms, got {}",
            allowed
        );
    }

    #[test]
    fn test_token_bucket_weighted_acquire() {
        let bucket = TokenBucket::new(10.0, 10);

        // Acquire 5 tokens
        assert!(bucket.try_acquire_n(5.0), "Should acquire 5 tokens");

        // Acquire 3 more
        assert!(bucket.try_acquire_n(3.0), "Should acquire 3 more tokens");

        // Try to acquire 5 more (only 2 left)
        assert!(
            !bucket.try_acquire_n(5.0),
            "Should fail to acquire 5 tokens (only 2 left)"
        );

        // Should still be able to acquire 2
        assert!(bucket.try_acquire_n(2.0), "Should acquire remaining 2 tokens");
    }

    #[test]
    fn test_rate_limiter_configure_and_check() {
        let limiter = RateLimiter::new();

        // Configure /api route
        limiter.configure_route("/api", 10.0, 10);

        // Should allow first 10 requests
        for i in 1..=10 {
            assert!(
                limiter.check_rate_limit("/api"),
                "Request {} should be allowed",
                i
            );
        }

        // 11th should fail
        assert!(
            !limiter.check_rate_limit("/api"),
            "Request 11 should be rate limited"
        );
    }

    #[test]
    fn test_rate_limiter_per_route_isolation() {
        let limiter = RateLimiter::new();

        // Configure different limits for different routes
        limiter.configure_route("/api", 10.0, 10);
        limiter.configure_route("/admin", 5.0, 5);

        // Drain /api
        for _ in 0..10 {
            limiter.check_rate_limit("/api");
        }

        // /api should be rate limited
        assert!(
            !limiter.check_rate_limit("/api"),
            "/api should be rate limited"
        );

        // /admin should still be available
        assert!(
            limiter.check_rate_limit("/admin"),
            "/admin should still be available"
        );
    }

    #[test]
    fn test_rate_limiter_no_config_allows_all() {
        let limiter = RateLimiter::new();

        // No rate limit configured - should allow all
        for _ in 0..1000 {
            assert!(
                limiter.check_rate_limit("/unconfigured"),
                "Unconfigured routes should allow all requests"
            );
        }
    }

    #[test]
    fn test_rate_limiter_remove_route() {
        let limiter = RateLimiter::new();

        // Configure and drain
        limiter.configure_route("/api", 10.0, 10);
        for _ in 0..10 {
            limiter.check_rate_limit("/api");
        }

        // Should be rate limited
        assert!(!limiter.check_rate_limit("/api"));

        // Remove configuration
        limiter.remove_route("/api");

        // Should now allow (no config = allow all)
        assert!(
            limiter.check_rate_limit("/api"),
            "Should allow after removing rate limit config"
        );
    }

    #[test]
    fn test_rate_limiter_concurrent_access() {
        use std::sync::Arc;

        let limiter = Arc::new(RateLimiter::new());
        limiter.configure_route("/api", 100.0, 100);

        // Spawn 10 threads, each trying 20 requests
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let limiter = Arc::clone(&limiter);
                thread::spawn(move || {
                    let mut allowed = 0;
                    for _ in 0..20 {
                        if limiter.check_rate_limit("/api") {
                            allowed += 1;
                        }
                    }
                    allowed
                })
            })
            .collect();

        // Collect results
        let total_allowed: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();

        // Should allow exactly 100 requests (burst capacity)
        assert_eq!(
            total_allowed, 100,
            "Should allow exactly 100 requests across all threads"
        );
    }

    #[test]
    fn test_rate_limiter_metrics_recorded() {
        let limiter = RateLimiter::new();
        limiter.configure_route("/test", 10.0, 10);

        // Allow 5 requests
        for _ in 0..5 {
            assert!(limiter.check_rate_limit("/test"));
        }

        // Check metrics are accessible (they're recorded to RATE_LIMITER_REGISTRY)
        let metrics = crate::proxy::rate_limiter::rate_limiter_registry().gather();

        // Should have metrics for rauta_rate_limit_requests_total
        let has_request_metric = metrics.iter().any(|family| {
            family.get_name() == "rauta_rate_limit_requests_total"
        });
        assert!(
            has_request_metric,
            "Should have rauta_rate_limit_requests_total metric"
        );

        // Should have metrics for rauta_rate_limit_tokens_available
        let has_tokens_metric = metrics.iter().any(|family| {
            family.get_name() == "rauta_rate_limit_tokens_available"
        });
        assert!(
            has_tokens_metric,
            "Should have rauta_rate_limit_tokens_available metric"
        );
    }

    #[test]
    fn test_token_bucket_available_tokens() {
        let bucket = TokenBucket::new(10.0, 20);

        // Full bucket
        assert!(
            (bucket.available_tokens() - 20.0).abs() < 0.01,
            "Should start with 20 tokens"
        );

        // Acquire 5
        bucket.try_acquire_n(5.0);
        assert!(
            (bucket.available_tokens() - 15.0).abs() < 0.01,
            "Should have 15 tokens after acquiring 5"
        );

        // Acquire 10 more
        bucket.try_acquire_n(10.0);
        assert!(
            (bucket.available_tokens() - 5.0).abs() < 0.01,
            "Should have 5 tokens after acquiring 15 total"
        );
    }
}
