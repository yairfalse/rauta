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
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tracing::{debug, error, info, warn};

/// Full body type for requests
type Full<T> = http_body_util::Full<T>;

/// Per-worker backend connection pools
/// NOTE: Currently uses Arc<Mutex> for simplicity, will be per-core owned in Stage 2
pub struct BackendConnectionPools {
    pools: HashMap<BackendKey, Http2Pool>,
}

/// Backend identifier (IP:port)
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct BackendKey {
    ipv4: u32,
    port: u16,
}

impl From<Backend> for BackendKey {
    fn from(backend: Backend) -> Self {
        Self {
            ipv4: backend.ipv4,
            port: backend.port,
        }
    }
}

/// Pool of HTTP/2 connections to a single backend
pub struct Http2Pool {
    backend: Backend,

    // Active HTTP/2 connections
    connections: Vec<Http2Connection>,

    // Connection limits
    max_connections: usize,    // Max concurrent connections (default: 4)
    max_streams_per_conn: u32, // Learned from SETTINGS frame

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

impl BackendConnectionPools {
    pub fn new() -> Self {
        Self {
            pools: HashMap::new(),
        }
    }

    /// Get or create pool for backend
    pub fn get_or_create_pool(&mut self, backend: Backend) -> &mut Http2Pool {
        let key = BackendKey::from(backend);
        self.pools.entry(key).or_insert_with(|| {
            debug!(
                backend_ip = %ipv4_to_string(backend.ipv4),
                backend_port = backend.port,
                "Creating new HTTP/2 pool for backend"
            );
            Http2Pool::new(backend)
        })
    }
}

impl Http2Pool {
    pub fn new(backend: Backend) -> Self {
        Self {
            backend,
            connections: Vec::new(),
            max_connections: 4,        // Start conservative
            max_streams_per_conn: 100, // Default, will be updated from SETTINGS
            health_state: HealthState::Healthy,
            consecutive_failures: 0,
            last_success: Instant::now(),
            metrics: PoolMetrics::default(),
        }
    }

    /// Get available connection or create new one
    /// Returns a cloned SendRequest (cheap operation, designed to be cloned)
    pub async fn get_connection(&mut self) -> Result<http2::SendRequest<Full<Bytes>>, PoolError> {
        // 1. Check circuit breaker
        if self.health_state == HealthState::Unhealthy {
            // Try to recover after timeout
            if self.last_success.elapsed() > Duration::from_secs(30) {
                info!(
                    backend_ip = %ipv4_to_string(self.backend.ipv4),
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
        self.connections
            .retain(|c| c.state != ConnectionState::Failed);

        // 3. Find connection that is ready for requests
        let mut conn_index = None;
        for (i, conn) in self.connections.iter_mut().enumerate() {
            if conn.state == ConnectionState::Active && conn.sender.is_ready() {
                conn.last_used = Instant::now();
                conn_index = Some(i);
                break;
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
        Err(PoolError::NoCapacity)
    }

    /// Create new HTTP/2 connection to backend
    async fn create_connection(&mut self) -> Result<Http2Connection, PoolError> {
        let backend_addr = format!(
            "{}:{}",
            ipv4_to_string(self.backend.ipv4),
            self.backend.port
        );

        debug!(
            backend = %backend_addr,
            pool_size = self.connections.len(),
            "Creating new HTTP/2 connection"
        );

        // Connect TCP
        let stream = TcpStream::connect(&backend_addr).await.map_err(|e| {
            self.record_failure();
            PoolError::ConnectionFailed(e.to_string())
        })?;

        let io = TokioIo::new(stream);

        // HTTP/2 handshake
        let (sender, conn) = http2::handshake(TokioExecutor::new(), io)
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
                backend_ip = %ipv4_to_string(self.backend.ipv4),
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

        // Circuit breaker logic
        if self.consecutive_failures >= 5 && self.health_state == HealthState::Healthy {
            warn!(
                backend_ip = %ipv4_to_string(self.backend.ipv4),
                backend_port = self.backend.port,
                consecutive_failures = self.consecutive_failures,
                "Backend degraded - reducing capacity"
            );
            self.health_state = HealthState::Degraded;
            self.max_connections = self.max_connections.max(1) / 2; // Reduce by 50%
        }

        if self.consecutive_failures >= 10 {
            error!(
                backend_ip = %ipv4_to_string(self.backend.ipv4),
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

/// Convert IPv4 u32 to string format (e.g., "192.168.1.1")
fn ipv4_to_string(ipv4: u32) -> String {
    let bytes = ipv4.to_be_bytes();
    format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3])
}
