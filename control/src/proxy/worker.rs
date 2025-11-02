//! Per-core worker architecture for lock-free performance
//!
//! Each CPU core gets its own worker with dedicated connection pools.
//! No Arc/Mutex on hot path = maximum throughput.

use crate::proxy::backend_pool::BackendConnectionPools;
use crate::proxy::router::Router;
use std::sync::Arc;

/// Per-core worker (owns connection pools, no sharing!)
pub struct Worker {
    #[allow(dead_code)] // Used in tests; will be used for routing
    id: usize,
    #[allow(dead_code)] // Will be used when server integrates workers
    router: Arc<Router>,
    #[allow(dead_code)] // Used in tests via pools() method
    pools: BackendConnectionPools, // OWNED, not Arc<Mutex>!
}

impl Worker {
    /// Create new worker for this core
    pub fn new(id: usize, router: Arc<Router>) -> Self {
        Self {
            id,
            router,
            pools: BackendConnectionPools::new(),
        }
    }

    /// Get worker ID (core number)
    #[allow(dead_code)] // Used in tests; will be used for debugging/metrics
    pub fn id(&self) -> usize {
        self.id
    }

    /// Get reference to connection pools (owned by this worker)
    #[allow(dead_code)] // Used in tests; will be used in request handling
    pub fn pools(&mut self) -> &mut BackendConnectionPools {
        &mut self.pools
    }

    /// Get HTTP/2 connection for backend (lock-free!)
    ///
    /// This is the hot path method - no Arc<Mutex> here!
    /// Each worker owns its pools, so this is just a mutable borrow.
    #[allow(dead_code)] // Will be used in request handling
    pub async fn get_backend_connection(
        &mut self,
        backend: common::Backend,
    ) -> Result<
        hyper::client::conn::http2::SendRequest<http_body_util::Full<hyper::body::Bytes>>,
        crate::proxy::backend_pool::PoolError,
    > {
        let pool = self.pools.get_or_create_pool(backend);
        pool.get_connection().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Backend, HttpMethod};
    use std::net::Ipv4Addr;

    /// RED: Test that each worker owns its pools (no Arc/Mutex)
    #[tokio::test]
    async fn test_worker_owns_pools() {
        let router = Arc::new(Router::new());

        // Create two workers
        let mut worker1 = Worker::new(0, router.clone());
        let mut worker2 = Worker::new(1, router.clone());

        assert_eq!(worker1.id(), 0);
        assert_eq!(worker2.id(), 1);

        // Each worker has its own pools (different memory addresses)
        let pools1_ptr = worker1.pools() as *mut BackendConnectionPools;
        let pools2_ptr = worker2.pools() as *mut BackendConnectionPools;

        assert_ne!(
            pools1_ptr, pools2_ptr,
            "Workers should have separate pool instances"
        );
    }

    /// RED: Test that worker can get connections without locks
    #[tokio::test]
    async fn test_worker_lock_free_connection() {
        let router = Arc::new(Router::new());
        // Use port 9999 which should not be in use
        let backend = Backend::new(u32::from(Ipv4Addr::new(127, 0, 0, 1)), 9999, 100);

        // Add route to router
        router
            .add_route(HttpMethod::GET, "/api/test", vec![backend])
            .expect("Should add route");

        let mut worker = Worker::new(0, router.clone());

        // Get pool for backend (no locks needed!)
        let pool = worker.pools().get_or_create_pool(backend);

        // Try to get connection (will fail since nothing is listening on 9999)
        let result = pool.get_connection().await;

        // Expected to fail, but should compile and run without Arc<Mutex>
        assert!(
            result.is_err(),
            "Should fail to connect (no backend running)"
        );
    }

    /// RED: Test multiple workers can operate independently
    #[tokio::test]
    async fn test_workers_independent() {
        let router = Arc::new(Router::new());
        // Use port 9999 which should not be in use
        let backend = Backend::new(u32::from(Ipv4Addr::new(127, 0, 0, 1)), 9999, 100);

        router
            .add_route(HttpMethod::GET, "/api/test", vec![backend])
            .expect("Should add route");

        // Spawn two workers on different "cores"
        let mut worker0 = Worker::new(0, router.clone());
        let mut worker1 = Worker::new(1, router.clone());

        // Each worker creates its own connection pool independently
        let pool0 = worker0.pools().get_or_create_pool(backend);
        let pool1 = worker1.pools().get_or_create_pool(backend);

        // Pools are separate (different memory)
        let pool0_ptr = pool0 as *const _;
        let pool1_ptr = pool1 as *const _;

        assert_ne!(pool0_ptr, pool1_ptr, "Each worker has separate pools");
    }
}
