//! HTTP/1.1 Backend Connection Pool
//!
//! Provides connection pooling for HTTP/1.1 backends to avoid
//! ephemeral port exhaustion under high load.
//!
//! Key features:
//! - Per-backend connection pools
//! - Keepalive connection reuse
//! - Idle connection cleanup
//! - Configurable pool size

// Allow dead_code - this module provides a generic connection pool implementation
// for testing and future use. The listener_manager currently uses hyper_util's
// built-in Client pooling, but this can be used for custom pooling scenarios.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Configuration for HTTP/1.1 connection pool
#[derive(Debug, Clone)]
pub struct Http1PoolConfig {
    /// Maximum idle connections per backend
    pub max_idle_per_backend: usize,
    /// Idle connection timeout
    pub idle_timeout: Duration,
}

impl Default for Http1PoolConfig {
    fn default() -> Self {
        Self {
            max_idle_per_backend: 16,
            idle_timeout: Duration::from_secs(60),
        }
    }
}

/// HTTP/1.1 connection pool manager
///
/// Generic over sender type S for testability
pub struct Http1Pool<S> {
    config: Http1PoolConfig,
    // Pool of idle connections per backend address
    pools: Mutex<HashMap<SocketAddr, Vec<PooledConnection<S>>>>,
}

/// A pooled connection with metadata
struct PooledConnection<S> {
    sender: S,
    #[allow(dead_code)]
    created_at: Instant,
    last_used: Instant,
}

impl<S> Http1Pool<S> {
    /// Create a new connection pool with default config
    pub fn new() -> Self {
        Self::with_config(Http1PoolConfig::default())
    }

    /// Create a new connection pool with custom config
    pub fn with_config(config: Http1PoolConfig) -> Self {
        Self {
            config,
            pools: Mutex::new(HashMap::new()),
        }
    }

    /// Try to get an idle connection for a backend
    ///
    /// Returns None if no idle connection is available
    pub async fn try_get(&self, backend: SocketAddr) -> Option<S> {
        let mut pools = self.pools.lock().await;
        let pool = pools.get_mut(&backend)?;

        // Get most recently used connection (LIFO for better cache locality)
        let conn = pool.pop()?;

        // Check if connection is still fresh
        if conn.last_used.elapsed() > self.config.idle_timeout {
            // Connection expired, drop it and return None
            return None;
        }

        Some(conn.sender)
    }

    /// Put a connection back into the pool
    ///
    /// If the pool is at capacity, the connection is dropped
    pub async fn put(&self, backend: SocketAddr, sender: S) {
        let mut pools = self.pools.lock().await;
        let pool = pools.entry(backend).or_insert_with(Vec::new);

        // Check capacity
        if pool.len() >= self.config.max_idle_per_backend {
            // Pool full, drop the connection (oldest will naturally be dropped
            // when we exceed capacity)
            return;
        }

        pool.push(PooledConnection {
            sender,
            created_at: Instant::now(),
            last_used: Instant::now(),
        });
    }

    /// Get pool statistics for a backend
    pub async fn stats(&self, backend: SocketAddr) -> PoolStats {
        let pools = self.pools.lock().await;
        let idle_count = pools.get(&backend).map(|p| p.len()).unwrap_or(0);
        PoolStats {
            idle_connections: idle_count,
            max_idle: self.config.max_idle_per_backend,
        }
    }

    /// Get total idle connections across all backends
    pub async fn total_idle(&self) -> usize {
        let pools = self.pools.lock().await;
        pools.values().map(|p| p.len()).sum()
    }
}

impl<S> Default for Http1Pool<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics for a backend's connection pool
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolStats {
    pub idle_connections: usize,
    pub max_idle: usize,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn test_addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), port)
    }

    /// Mock HTTP/1.1 sender for testing
    struct MockSender {
        id: u64,
    }

    impl MockSender {
        fn new() -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            Self {
                id: COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            }
        }
    }

    #[tokio::test]
    async fn test_pool_creation_with_default_config() {
        let pool: Http1Pool<MockSender> = Http1Pool::new();
        assert_eq!(pool.config.max_idle_per_backend, 16);
        assert_eq!(pool.config.idle_timeout, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn test_pool_creation_with_custom_config() {
        let config = Http1PoolConfig {
            max_idle_per_backend: 8,
            idle_timeout: Duration::from_secs(30),
        };
        let pool: Http1Pool<MockSender> = Http1Pool::with_config(config);
        assert_eq!(pool.config.max_idle_per_backend, 8);
        assert_eq!(pool.config.idle_timeout, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn test_empty_pool_stats() {
        let pool: Http1Pool<MockSender> = Http1Pool::new();
        let stats = pool.stats(test_addr(8080)).await;
        assert_eq!(
            stats,
            PoolStats {
                idle_connections: 0,
                max_idle: 16,
            }
        );
    }

    #[tokio::test]
    async fn test_total_idle_empty() {
        let pool: Http1Pool<MockSender> = Http1Pool::new();
        assert_eq!(pool.total_idle().await, 0);
    }

    #[tokio::test]
    async fn test_pool_get_returns_none_when_empty() {
        let pool: Http1Pool<MockSender> = Http1Pool::new();
        let conn = pool.try_get(test_addr(8080)).await;
        assert!(conn.is_none(), "Empty pool should return None");
    }

    #[tokio::test]
    async fn test_pool_put_and_get() {
        let pool: Http1Pool<MockSender> = Http1Pool::new();
        let addr = test_addr(8080);

        // Put a mock connection
        pool.put(addr, MockSender::new()).await;

        // Stats should show 1 idle
        let stats = pool.stats(addr).await;
        assert_eq!(stats.idle_connections, 1);

        // Get should return the connection
        let conn = pool.try_get(addr).await;
        assert!(conn.is_some(), "Pool should return connection");

        // Stats should show 0 idle after get
        let stats = pool.stats(addr).await;
        assert_eq!(stats.idle_connections, 0);
    }

    #[tokio::test]
    async fn test_pool_respects_max_idle() {
        let config = Http1PoolConfig {
            max_idle_per_backend: 2,
            idle_timeout: Duration::from_secs(60),
        };
        let pool: Http1Pool<MockSender> = Http1Pool::with_config(config);
        let addr = test_addr(8080);

        // Put 3 connections, only 2 should be kept
        pool.put(addr, MockSender::new()).await;
        pool.put(addr, MockSender::new()).await;
        pool.put(addr, MockSender::new()).await;

        let stats = pool.stats(addr).await;
        assert_eq!(stats.idle_connections, 2, "Pool should cap at max_idle");
    }

    #[tokio::test]
    async fn test_pool_per_backend_isolation() {
        let pool: Http1Pool<MockSender> = Http1Pool::new();
        let addr1 = test_addr(8080);
        let addr2 = test_addr(8081);

        pool.put(addr1, MockSender::new()).await;
        pool.put(addr1, MockSender::new()).await;
        pool.put(addr2, MockSender::new()).await;

        assert_eq!(pool.stats(addr1).await.idle_connections, 2);
        assert_eq!(pool.stats(addr2).await.idle_connections, 1);
        assert_eq!(pool.total_idle().await, 3);
    }

    #[tokio::test]
    async fn test_pool_returns_same_connection() {
        let pool: Http1Pool<MockSender> = Http1Pool::new();
        let addr = test_addr(8080);

        let sender = MockSender::new();
        let original_id = sender.id;

        pool.put(addr, sender).await;
        let returned = pool.try_get(addr).await.expect("Should get connection");

        assert_eq!(returned.id, original_id, "Should return same connection");
    }
}
