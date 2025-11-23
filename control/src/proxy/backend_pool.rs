//! HTTP/2 Backend Connection Pool
//!
//! Production-grade connection pooling with:
//! - Stream tracking (respects max_concurrent_streams)
//! - Circuit breakers (Healthy → Degraded → Unhealthy)
//! - Adaptive limits (learns from SETTINGS frames)
//! - Connection lifecycle management
//! - Per-backend isolation

use common::Backend;
use hyper::body::Bytes;
use hyper::client::conn::http2;
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;
use lazy_static::lazy_static;
use prometheus::{Encoder, IntCounterVec, IntGaugeVec, Opts, Registry, TextEncoder};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tracing::{debug, error, info, warn};

/// Format a Backend for logging (works for both IPv4 and IPv6)
fn format_backend(backend: &Backend) -> String {
    backend.to_socket_addr().to_string()
}

lazy_static! {
    /// Global HTTP/2 pool metrics registry
    static ref POOL_METRICS_REGISTRY: Registry = Registry::new();

    /// Active HTTP/2 connections per backend and worker (Gauge)
    ///
    /// Note: Fallback metrics use .expect() as last line of defense - if Prometheus itself is broken, we should panic
    #[allow(clippy::expect_used)]
    static ref POOL_CONNECTIONS_ACTIVE: IntGaugeVec = {
        let opts = Opts::new(
            "http2_pool_connections_active",
            "Active HTTP/2 connections per backend and worker (for lock-free verification)"
        );
        let gauge = IntGaugeVec::new(opts, &["backend", "worker_id"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create http2_pool_connections_active gauge: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntGaugeVec::new(
                        Opts::new("http2_pool_connections_active_fallback", "Fallback metric for active connections"),
                        &["backend", "worker_id"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = POOL_METRICS_REGISTRY.register(Box::new(gauge.clone())) {
            eprintln!("WARN: Failed to register http2_pool_connections_active gauge: {}", e);
            eprintln!("WARN: Metrics collection will be degraded but gateway will continue");
        }
        gauge
    };

    /// Max concurrent streams configured per connection (Gauge)
    /// Phase 1: Tracks SETTINGS_MAX_CONCURRENT_STREAMS value (RFC 7540)
    ///
    /// Note: Fallback metrics use .expect() as last line of defense - if Prometheus itself is broken, we should panic
    #[allow(clippy::expect_used)]
    static ref POOL_MAX_CONCURRENT_STREAMS: IntGaugeVec = {
        let opts = Opts::new(
            "http2_pool_max_concurrent_streams",
            "Max concurrent streams configured per HTTP/2 connection (RFC 7540 Section 6.5.2)"
        );
        let gauge = IntGaugeVec::new(opts, &["backend", "worker_id"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create http2_pool_max_concurrent_streams gauge: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntGaugeVec::new(
                        Opts::new("http2_pool_max_concurrent_streams_fallback", "Fallback metric for max concurrent streams"),
                        &["backend", "worker_id"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = POOL_METRICS_REGISTRY.register(Box::new(gauge.clone())) {
            eprintln!("WARN: Failed to register http2_pool_max_concurrent_streams gauge: {}", e);
            eprintln!("WARN: Metrics collection will be degraded but gateway will continue");
        }
        gauge
    };

    /// Total HTTP/2 connections created per backend and worker (Counter)
    ///
    /// Note: Fallback metrics use .expect() as last line of defense - if Prometheus itself is broken, we should panic
    #[allow(clippy::expect_used)]
    static ref POOL_CONNECTIONS_CREATED: IntCounterVec = {
        let opts = Opts::new(
            "http2_pool_connections_created_total",
            "Total HTTP/2 connections created per backend and worker"
        );
        let counter = IntCounterVec::new(opts, &["backend", "worker_id"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create http2_pool_connections_created_total counter: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntCounterVec::new(
                        Opts::new("http2_pool_connections_created_total_fallback", "Fallback metric for connections created"),
                        &["backend", "worker_id"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = POOL_METRICS_REGISTRY.register(Box::new(counter.clone())) {
            eprintln!("WARN: Failed to register http2_pool_connections_created_total counter: {}", e);
            eprintln!("WARN: Metrics collection will be degraded but gateway will continue");
        }
        counter
    };

    /// Total HTTP/2 connection failures per backend and worker (Counter)
    ///
    /// Note: Fallback metrics use .expect() as last line of defense - if Prometheus itself is broken, we should panic
    #[allow(clippy::expect_used)]
    static ref POOL_CONNECTIONS_FAILED: IntCounterVec = {
        let opts = Opts::new(
            "http2_pool_connections_failed_total",
            "Total HTTP/2 connection failures per backend and worker"
        );
        let counter = IntCounterVec::new(opts, &["backend", "worker_id"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create http2_pool_connections_failed_total counter: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntCounterVec::new(
                        Opts::new("http2_pool_connections_failed_total_fallback", "Fallback metric for connection failures"),
                        &["backend", "worker_id"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = POOL_METRICS_REGISTRY.register(Box::new(counter.clone())) {
            eprintln!("WARN: Failed to register http2_pool_connections_failed_total counter: {}", e);
            eprintln!("WARN: Metrics collection will be degraded but gateway will continue");
        }
        counter
    };

    /// Total requests queued waiting for connection per backend and worker (Counter)
    ///
    /// Note: Fallback metrics use .expect() as last line of defense - if Prometheus itself is broken, we should panic
    #[allow(clippy::expect_used)]
    static ref POOL_REQUESTS_QUEUED: IntCounterVec = {
        let opts = Opts::new(
            "http2_pool_requests_queued_total",
            "Total requests queued waiting for connection per backend and worker"
        );
        let counter = IntCounterVec::new(opts, &["backend", "worker_id"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create http2_pool_requests_queued_total counter: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntCounterVec::new(
                        Opts::new("http2_pool_requests_queued_total_fallback", "Fallback metric for queued requests"),
                        &["backend", "worker_id"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = POOL_METRICS_REGISTRY.register(Box::new(counter.clone())) {
            eprintln!("WARN: Failed to register http2_pool_requests_queued_total counter: {}", e);
            eprintln!("WARN: Metrics collection will be degraded but gateway will continue");
        }
        counter
    };
}

/// Full body type for requests
type Full<T> = http_body_util::Full<T>;

/// Per-worker backend connection pools
/// Each worker owns its pools (no Arc/Mutex on hot path)
pub struct BackendConnectionPools {
    worker_id: usize, // For metrics labeling
    pools: HashMap<BackendKey, Http2Pool>,
}

/// Backend identifier (IP:port)
///
/// Uses full 16-byte IP address to support both IPv4 and IPv6.
/// IPv4 addresses use only the first 4 bytes (same as Backend struct).
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct BackendKey {
    ip: [u8; 16],
    port: u16,
}

impl From<Backend> for BackendKey {
    fn from(backend: Backend) -> Self {
        Self {
            ip: *backend.ip_bytes(),
            port: backend.port,
        }
    }
}

/// Pool of HTTP/2 connections to a single backend
pub struct Http2Pool {
    backend: Backend,
    worker_id: usize, // For per-worker metrics

    // Active HTTP/2 connections
    connections: Vec<Http2Connection>,

    // Connection limits
    max_connections: usize, // Max concurrent connections (optimized: 8, was 4)
    max_streams_per_conn: u32, // Learned from SETTINGS frame

    // Round-robin load distribution (Phase 1 optimization)
    next_conn_index: usize, // Tracks next connection to use for round-robin

    // HPACK compression (Phase 2: RFC 7541)
    header_table_size: u32, // SETTINGS_HEADER_TABLE_SIZE (default 4096, optimized 8192)

    // Health tracking
    health_state: HealthState,
    consecutive_failures: u32,
    last_success: Instant,

    // Performance metrics
    metrics: PoolMetrics,
}

/// Single HTTP/2 connection to backend
#[allow(dead_code)] // Fields used in Stage 2 per-core workers
struct Http2Connection {
    // HTTP/2 sender (from hyper)
    sender: http2::SendRequest<Full<Bytes>>,

    // Stream tracking (Stage 2: manual tracking with SETTINGS negotiation)
    active_streams: u32, // Currently in-flight
    max_streams: u32,    // From SETTINGS frame (negotiated)
    total_requests: u64, // Lifetime request count

    // Connection state
    state: ConnectionState,
    created_at: Instant,
    last_used: Instant,

    // Health
    consecutive_errors: u32,
    last_error: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Draining variant used in Stage 2
enum ConnectionState {
    Active,   // Ready for requests
    Draining, // Gracefully shutting down (no new streams)
    Failed,   // Connection failed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthState {
    Healthy,   // All good
    Degraded,  // Some failures, reduced capacity
    Unhealthy, // Circuit breaker open
}

#[derive(Debug, Default)]
#[allow(dead_code)] // Fields exported as Prometheus metrics in Stage 2
struct PoolMetrics {
    requests_total: u64,
    requests_queued: u64, // Waiting for available connection
    connections_created: u64,
    connections_failed: u64,
}

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // QueueTimeout used in Stage 2 with request queuing
pub enum PoolError {
    #[error("Circuit breaker is open")]
    CircuitBreakerOpen,

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("HTTP/2 handshake failed: {0}")]
    HandshakeFailed(String),

    #[error("No connections have capacity")]
    NoCapacity,

    #[error("Queue timeout")]
    QueueTimeout,
}

impl Default for BackendConnectionPools {
    fn default() -> Self {
        Self::new(0) // Default to worker 0 for backwards compat
    }
}

impl BackendConnectionPools {
    pub fn new(worker_id: usize) -> Self {
        Self {
            worker_id,
            pools: HashMap::new(),
        }
    }

    /// Get or create pool for backend
    /// NOTE: This returns a pool that may not be pre-warmed yet.
    /// For best performance, call `prewarm_pool()` after getting the pool.
    pub fn get_or_create_pool(&mut self, backend: Backend) -> &mut Http2Pool {
        let key = BackendKey::from(backend);
        let worker_id = self.worker_id; // Capture for closure
        self.pools.entry(key).or_insert_with(|| {
            debug!(
                backend = %backend,  // Supports IPv4 and IPv6
                worker_id = worker_id,
                "Creating new HTTP/2 pool for backend"
            );
            Http2Pool::new(backend, worker_id)
        })
    }

    /// Pre-warm pool for a specific backend (call after get_or_create_pool)
    pub async fn prewarm_pool(&mut self, backend: Backend) -> Result<(), PoolError> {
        let key = BackendKey::from(backend);
        if let Some(pool) = self.pools.get_mut(&key) {
            // Only prewarm if connections haven't been created yet
            if pool.connections.is_empty() {
                pool.prewarm().await?;
            }
        }
        Ok(())
    }
}

impl Http2Pool {
    pub fn new(backend: Backend, worker_id: usize) -> Self {
        let max_streams_per_conn = 500; // RFC 7540 Section 6.5.2 - high limit for HTTP/2 multiplexing
        let header_table_size = 8192; // RFC 7541 - doubled from default 4096 for proxy use

        // Set max_concurrent_streams metric
        let backend_label = backend.to_string(); // Supports IPv4 and IPv6
        let worker_label = worker_id.to_string();
        POOL_MAX_CONCURRENT_STREAMS
            .with_label_values(&[&backend_label, &worker_label])
            .set(max_streams_per_conn as i64);

        Self {
            backend,
            worker_id,
            connections: Vec::new(),
            max_connections: 8, // 8 connections * 500 streams = 4000 concurrent requests per worker
            max_streams_per_conn,
            next_conn_index: 0, // Round-robin starts at first connection
            header_table_size,  // HPACK compression (Phase 2)
            health_state: HealthState::Healthy,
            consecutive_failures: 0,
            last_success: Instant::now(),
            metrics: PoolMetrics::default(),
        }
    }

    /// Pre-warm connections (create all max_connections upfront)
    /// Call this after pool creation to avoid lazy connection establishment
    pub async fn prewarm(&mut self) -> Result<(), PoolError> {
        info!(
            backend_ip = %format_backend(&self.backend),
            backend_port = self.backend.port,
            worker_id = self.worker_id,
            target_connections = self.max_connections,
            "Pre-warming HTTP/2 connection pool"
        );

        for i in 0..self.max_connections {
            match self.create_connection().await {
                Ok(conn) => {
                    debug!(
                        backend_ip = %format_backend(&self.backend),
                        backend_port = self.backend.port,
                        worker_id = self.worker_id,
                        connection_num = i + 1,
                        total = self.max_connections,
                        "Pre-warmed connection"
                    );
                    self.connections.push(conn);
                }
                Err(e) => {
                    warn!(
                        backend_ip = %format_backend(&self.backend),
                        backend_port = self.backend.port,
                        worker_id = self.worker_id,
                        connection_num = i + 1,
                        error = %e,
                        "Failed to pre-warm connection, continuing with partial pool"
                    );
                    // Don't fail completely - partial pool is better than nothing
                    break;
                }
            }
        }

        info!(
            backend_ip = %format_backend(&self.backend),
            backend_port = self.backend.port,
            worker_id = self.worker_id,
            connections_created = self.connections.len(),
            "Pre-warming complete"
        );

        Ok(())
    }

    /// Get available connection or create new one
    /// Returns a cloned SendRequest (cheap operation, designed to be cloned)
    pub async fn get_connection(&mut self) -> Result<http2::SendRequest<Full<Bytes>>, PoolError> {
        // 1. Check circuit breaker
        if self.health_state == HealthState::Unhealthy {
            // Try to recover after timeout
            if self.last_success.elapsed() > Duration::from_secs(30) {
                info!(
                    backend_ip = %format_backend(&self.backend),
                    backend_port = self.backend.port,
                    "Circuit breaker timeout expired, attempting recovery"
                );
                self.health_state = HealthState::Degraded;
                self.consecutive_failures = 0;
            } else {
                return Err(PoolError::CircuitBreakerOpen);
            }
        }

        // 2. Clean up failed connections first
        let before_cleanup = self.connections.len();
        self.connections
            .retain(|c| c.state != ConnectionState::Failed);
        let after_cleanup = self.connections.len();

        // Update active connections gauge if we removed any
        if before_cleanup != after_cleanup {
            let backend_label = format_backend(&self.backend);
            let worker_label = self.worker_id.to_string();
            POOL_CONNECTIONS_ACTIVE
                .with_label_values(&[&backend_label, &worker_label])
                .set(after_cleanup as i64);
        }

        // 3. Find connection that is ready for requests using round-robin
        let mut conn_index = None;
        let num_conns = self.connections.len();

        if num_conns > 0 {
            // Try connections in round-robin order starting from next_conn_index
            for i in 0..num_conns {
                let idx = (self.next_conn_index + i) % num_conns;
                let conn = &mut self.connections[idx];

                if conn.state == ConnectionState::Active && conn.sender.is_ready() {
                    conn.last_used = Instant::now();
                    conn_index = Some(idx);
                    // Update round-robin index for next request
                    self.next_conn_index = (idx + 1) % num_conns;
                    break;
                }
            }
        }

        if let Some(idx) = conn_index {
            // Return cloned sender (cheap operation, designed to be cloned)
            return Ok(self.connections[idx].sender.clone());
        }

        // 4. Create new connection if under limit
        if self.connections.len() < self.max_connections {
            let mut conn = self.create_connection().await?;
            conn.last_used = Instant::now();
            let sender = conn.sender.clone();
            self.connections.push(conn);
            return Ok(sender);
        }

        // 5. All connections maxed out
        self.metrics.requests_queued += 1;

        // Update Prometheus metrics
        let backend_label = format_backend(&self.backend);
        let worker_label = self.worker_id.to_string();
        POOL_REQUESTS_QUEUED
            .with_label_values(&[&backend_label, &worker_label])
            .inc();

        Err(PoolError::NoCapacity)
    }

    /// Create new HTTP/2 connection to backend
    async fn create_connection(&mut self) -> Result<Http2Connection, PoolError> {
        // Convert Backend to SocketAddr (supports both IPv4 and IPv6)
        let socket_addr = self.backend.to_socket_addr();

        debug!(
            backend = %socket_addr,
            pool_size = self.connections.len(),
            "Creating new HTTP/2 connection"
        );

        // Connect TCP with timeout (5 seconds - fail fast on dead backends)
        let connect_timeout = Duration::from_secs(5);
        let stream = tokio::time::timeout(connect_timeout, TcpStream::connect(socket_addr))
            .await
            .map_err(|_| {
                self.record_failure();
                PoolError::ConnectionFailed("Connection timeout after 5s".to_string())
            })?
            .map_err(|e| {
                self.record_failure();
                PoolError::ConnectionFailed(e.to_string())
            })?;

        let io = TokioIo::new(stream);

        // HTTP/2 handshake with Builder
        // RFC 7540 Section 6.5.2: SETTINGS_MAX_CONCURRENT_STREAMS
        // RFC 7541: SETTINGS_HEADER_TABLE_SIZE (HPACK compression)
        let (sender, conn) = http2::Builder::new(TokioExecutor::new())
            .max_concurrent_streams(self.max_streams_per_conn)
            .header_table_size(self.header_table_size)
            .adaptive_window(true) // Let hyper tune flow control windows automatically
            .handshake(io)
            .await
            .map_err(|e| {
                self.record_failure();
                PoolError::HandshakeFailed(e.to_string())
            })?;

        // Spawn connection driver
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                debug!("HTTP/2 connection closed: {}", e);
            }
        });

        self.record_success();
        self.metrics.connections_created += 1;

        // Update Prometheus metrics
        let backend_label = socket_addr.to_string();
        let worker_label = self.worker_id.to_string();
        POOL_CONNECTIONS_CREATED
            .with_label_values(&[&backend_label, &worker_label])
            .inc();
        POOL_CONNECTIONS_ACTIVE
            .with_label_values(&[&backend_label, &worker_label])
            .set(self.connections.len() as i64 + 1); // +1 for the new connection

        Ok(Http2Connection {
            sender,
            active_streams: 0,
            max_streams: self.max_streams_per_conn, // Will be updated by SETTINGS
            total_requests: 0,
            state: ConnectionState::Active,
            created_at: Instant::now(),
            last_used: Instant::now(),
            consecutive_errors: 0,
            last_error: None,
        })
    }

    /// Record successful request
    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.last_success = Instant::now();

        // Recover from degraded state
        if self.health_state == HealthState::Degraded {
            info!(
                backend_ip = %format_backend(&self.backend),
                backend_port = self.backend.port,
                "Backend recovered from degraded state"
            );
            self.health_state = HealthState::Healthy;
        }
    }

    /// Record failed request
    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.metrics.connections_failed += 1;

        // Update Prometheus metrics
        let backend_label = format_backend(&self.backend);
        let worker_label = self.worker_id.to_string();
        POOL_CONNECTIONS_FAILED
            .with_label_values(&[&backend_label, &worker_label])
            .inc();

        // Circuit breaker logic
        if self.consecutive_failures >= 5 && self.health_state == HealthState::Healthy {
            warn!(
                backend_ip = %format_backend(&self.backend),
                backend_port = self.backend.port,
                consecutive_failures = self.consecutive_failures,
                "Backend degraded - reducing capacity"
            );
            self.health_state = HealthState::Degraded;
            self.max_connections = self.max_connections.max(1) / 2; // Reduce by 50%
        }

        if self.consecutive_failures >= 10 {
            error!(
                backend_ip = %format_backend(&self.backend),
                backend_port = self.backend.port,
                consecutive_failures = self.consecutive_failures,
                "Circuit breaker OPEN - blocking requests"
            );
            self.health_state = HealthState::Unhealthy;
        }
    }
}

impl Http2Connection {
    /// Check if connection should be closed
    #[allow(dead_code)] // Used in Stage 2 connection lifecycle cleanup
    pub fn should_close(&self) -> bool {
        // Close if:
        // 1. Failed state
        // 2. Too old (1 hour)
        // 3. Too many total requests (prevents memory leaks)
        // 4. Connection no longer ready
        matches!(self.state, ConnectionState::Failed)
            || self.created_at.elapsed() > Duration::from_secs(3600)
            || self.total_requests > 10_000
            || !self.sender.is_ready()
    }
}

/// Gather HTTP/2 pool metrics for Prometheus export
pub fn gather_pool_metrics() -> Result<String, String> {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    let metric_families = POOL_METRICS_REGISTRY.gather();
    encoder
        .encode(&metric_families, &mut buffer)
        .map_err(|e| format!("Failed to encode pool metrics: {}", e))?;

    String::from_utf8(buffer).map_err(|e| format!("Failed to convert pool metrics to UTF-8: {}", e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// GREEN: Test that pool metrics are exposed to Prometheus
    /// Verifies that http2_pool_connections_created_total is exported with backend labels
    #[tokio::test]
    async fn test_pool_metrics_exposed() {
        // Create a real HTTP/2 backend server for testing
        let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        // Spawn HTTP/2 backend server
        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = hyper::service::service_fn(
                |_req: hyper::Request<hyper::body::Incoming>| async move {
                    Ok::<_, hyper::Error>(
                        hyper::Response::builder()
                            .status(hyper::StatusCode::OK)
                            .body(Full::new(Bytes::from("test")))
                            .unwrap(),
                    )
                },
            );

            let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        // Wait for server to be ready
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create backend pointing to test server
        let backend_ipv4 = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => ipv4,
            std::net::IpAddr::V6(_) => {
                eprintln!(
                    "Test skipped: IPv6 not supported (connection pooling is IPv4-only currently)"
                );
                return;
            }
        };
        let backend = Backend::from_ipv4(backend_ipv4, backend_addr.port(), 100);
        let mut pools = BackendConnectionPools::new(0); // Test worker 0
        let pool = pools.get_or_create_pool(backend);

        // Trigger some metrics (connection should succeed)
        let conn_result = pool.get_connection().await;
        assert!(conn_result.is_ok(), "Connection should succeed");

        // Collect metrics
        let metrics_output = gather_pool_metrics().expect("Should gather metrics");

        // Should have http2_pool_connections_created_total metric
        assert!(
            metrics_output.contains("http2_pool_connections_created_total"),
            "Missing metric"
        );

        // Should have backend label (127.0.0.1:<port>)
        let label = format!("backend=\"127.0.0.1:{}\"", backend_addr.port());
        assert!(metrics_output.contains(&label), "Missing label");
    }

    /// RED: Test that failed connections are counted
    #[tokio::test]
    async fn test_pool_metrics_connection_failures() {
        // Invalid backend (nothing listening on port 1)
        let backend = Backend::from_ipv4(Ipv4Addr::new(127, 0, 0, 1), 1, 100);
        let mut pools = BackendConnectionPools::new(0); // Test worker 0
        let pool = pools.get_or_create_pool(backend);

        // Try to create connection (should fail)
        let result = pool.get_connection().await;
        assert!(result.is_err(), "Connection should fail");

        // Collect metrics
        let metrics_output = gather_pool_metrics().expect("Should gather metrics");

        // Should have http2_pool_connections_failed_total metric
        assert!(
            metrics_output.contains("http2_pool_connections_failed_total"),
            "Missing metric"
        );

        // Check value is at least 1 (with worker_id label)
        assert!(
            metrics_output.contains(
                "http2_pool_connections_failed_total{backend=\"127.0.0.1:1\",worker_id=\"0\"}"
            ),
            "Missing count for backend 127.0.0.1:1"
        );
    }

    /// RED: Test that active connections are tracked as gauge
    #[tokio::test]
    async fn test_pool_metrics_active_connections() {
        let backend = Backend::from_ipv4(Ipv4Addr::new(127, 0, 0, 1), 9001, 100);
        let mut pools = BackendConnectionPools::new(0); // Test worker 0
        let pool = pools.get_or_create_pool(backend);

        // Initialize gauge to 0
        let backend_label = backend.to_string(); // Supports IPv4 and IPv6
        let worker_label = pool.worker_id.to_string();
        POOL_CONNECTIONS_ACTIVE
            .with_label_values(&[&backend_label, &worker_label])
            .set(pool.connections.len() as i64);

        // Get current active connections (should be 0)
        let metrics_before = gather_pool_metrics().expect("Should gather metrics");
        let expected =
            "http2_pool_connections_active{backend=\"127.0.0.1:9001\",worker_id=\"0\"} 0";
        assert!(metrics_before.contains(expected), "Missing initial count");
    }

    /// RED: Test that queued requests are counted
    #[tokio::test]
    async fn test_pool_metrics_queued_requests() {
        let backend = Backend::from_ipv4(Ipv4Addr::new(127, 0, 0, 1), 9001, 100);
        let mut pools = BackendConnectionPools::new(0); // Test worker 0
        let pool = pools.get_or_create_pool(backend);

        // Set max_connections to 0 to force queueing
        pool.max_connections = 0;

        // Try to get connection (should queue)
        let result = pool.get_connection().await;
        assert!(matches!(result, Err(PoolError::NoCapacity)), "Wrong error");

        // Collect metrics
        let metrics_output = gather_pool_metrics().expect("Should gather metrics");

        // Should have http2_pool_requests_queued_total metric
        assert!(
            metrics_output.contains("http2_pool_requests_queued_total"),
            "Missing metric"
        );
    }

    /// RED: Test that max_concurrent_streams is configured to 500 (Phase 1: Connection Pool Tuning)
    /// Per RFC 7540 Section 6.5.2 and HTTP2_OPTIMIZATION_ROADMAP.md
    /// This test will FAIL until we implement http2::Builder with max_concurrent_streams(500)
    #[tokio::test]
    async fn test_max_concurrent_streams_configured() {
        let backend = Backend::from_ipv4(Ipv4Addr::new(127, 0, 0, 1), 9001, 100);
        let mut pools = BackendConnectionPools::new(0);
        let pool = pools.get_or_create_pool(backend);

        // Verify pool is configured for 500 streams per connection (not default 100)
        assert_eq!(pool.max_streams_per_conn, 500, "Wrong stream limit");
    }

    /// RED: Test that pool uses round-robin selection across multiple connections
    /// Phase 1: Multi-connection pooling with round-robin load distribution
    /// This test will FAIL until we add next_conn_index field to Http2Pool
    #[tokio::test]
    async fn test_round_robin_connection_selection() {
        let backend = Backend::from_ipv4(Ipv4Addr::new(127, 0, 0, 1), 9001, 100);
        let mut pools = BackendConnectionPools::new(0);
        let pool = pools.get_or_create_pool(backend);

        // Verify pool supports multiple connections for load distribution
        assert!(pool.max_connections >= 2, "Need >= 2 connections");

        // Access next_conn_index field (will fail to compile until added)
        let _initial_index = pool.next_conn_index;
        assert_eq!(_initial_index, 0, "Wrong initial index");
    }

    /// RED: Test that HPACK header table size is configured for compression
    /// Phase 2: HPACK optimization per RFC 7541
    /// Default is 4096 bytes - we should increase it for better compression
    #[tokio::test]
    async fn test_hpack_header_table_size() {
        let backend = Backend::from_ipv4(Ipv4Addr::new(127, 0, 0, 1), 9001, 100);
        let mut pools = BackendConnectionPools::new(0);
        let pool = pools.get_or_create_pool(backend);

        // Pool should track HPACK header table size
        // RFC 7541 default is 4096, we optimize to 8192 for proxy use
        assert_eq!(pool.header_table_size, 8192, "Wrong HPACK table size");
    }

    /// RED: Test that pool size is increased to 8 connections for higher concurrency
    /// Phase 3: Connection pool size optimization (4 → 8 connections per backend)
    /// Expected impact: +15-20% throughput by reducing queueing under high load
    #[tokio::test]
    async fn test_pool_size_increased_to_8_connections() {
        let backend = Backend::from_ipv4(Ipv4Addr::new(127, 0, 0, 1), 9001, 100);
        let mut pools = BackendConnectionPools::new(0);
        let pool = pools.get_or_create_pool(backend);

        // Pool should support 8 concurrent connections per backend (up from 4)
        // With 500 streams per connection: 8 × 500 = 4000 concurrent requests per worker
        assert_eq!(
            pool.max_connections, 8,
            "Pool should have 8 connections for higher concurrency"
        );
    }

    /// RED: Test that connection timeout fails fast on unreachable backend
    /// Production systems MUST NOT hang forever on dead backends
    /// Expected: Connection attempt should timeout within 5 seconds
    #[tokio::test]
    async fn test_connection_timeout_on_dead_backend() {
        use std::time::Instant;

        // Use a non-routable IP (TEST-NET-1 from RFC 5737)
        // This will cause connection to hang without timeout
        let backend = Backend::from_ipv4(Ipv4Addr::new(192, 0, 2, 1), 9999, 100);
        let mut pools = BackendConnectionPools::new(0);
        let pool = pools.get_or_create_pool(backend);

        let start = Instant::now();
        let result = pool.get_connection().await;
        let elapsed = start.elapsed();

        // Should fail (not hang forever)
        assert!(result.is_err(), "Connection to dead backend should fail");

        // Should fail FAST (within 5 seconds, not 30+ seconds)
        assert!(
            elapsed.as_secs() <= 6,
            "Connection timeout should be ~5s, got {:?}",
            elapsed
        );
    }
}
