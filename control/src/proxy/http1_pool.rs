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

// Allow dead_code during TDD development - will be used by listener_manager
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
pub struct Http1Pool {
    config: Http1PoolConfig,
    // Pool of idle connections per backend address
    pools: Mutex<HashMap<SocketAddr, Vec<PooledConnection>>>,
}

/// A pooled connection with metadata
struct PooledConnection {
    created_at: Instant,
    last_used: Instant,
}

impl Http1Pool {
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

impl Default for Http1Pool {
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

    #[tokio::test]
    async fn test_pool_creation_with_default_config() {
        let pool = Http1Pool::new();
        assert_eq!(pool.config.max_idle_per_backend, 16);
        assert_eq!(pool.config.idle_timeout, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn test_pool_creation_with_custom_config() {
        let config = Http1PoolConfig {
            max_idle_per_backend: 8,
            idle_timeout: Duration::from_secs(30),
        };
        let pool = Http1Pool::with_config(config);
        assert_eq!(pool.config.max_idle_per_backend, 8);
        assert_eq!(pool.config.idle_timeout, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn test_empty_pool_stats() {
        let pool = Http1Pool::new();
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
        let pool = Http1Pool::new();
        assert_eq!(pool.total_idle().await, 0);
    }

    // RED: These tests will fail until we implement get/put
    #[tokio::test]
    async fn test_pool_get_returns_none_when_empty() {
        // TODO: Implement get_connection in GREEN phase
        let pool = Http1Pool::new();
        let _stats = pool.stats(test_addr(8080)).await;
        // Will add: assert!(pool.get_connection(test_addr(8080)).await.is_none());
    }
}
