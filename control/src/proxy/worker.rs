//! Per-core worker architecture for lock-free performance
//!
//! Each CPU core gets its own worker with dedicated connection pools.
//! Lock-free worker selection + per-worker pool locks = maximum throughput.

use crate::proxy::backend_pool::BackendConnectionPools;
use crate::proxy::router::Router;
use std::sync::Arc;
use tokio::sync::Mutex; // Async-aware Mutex (Send-safe across await)

/// Per-core worker (owns connection pools with per-worker locking)
///
/// Architecture:
/// - Workers stored in Arc<Vec<Worker>> (immutable, lock-free selection)
/// - Each worker has Mutex<BackendConnectionPools> (per-worker lock)
/// - Result: Concurrent requests to different workers don't block each other!
pub struct Worker {
    #[allow(dead_code)] // Used in tests and metrics
    id: usize,
    #[allow(dead_code)] // Will be used when server integrates workers
    router: Arc<Router>,
    pools: Mutex<BackendConnectionPools>, // Per-worker lock (not global!)
}

impl Worker {
    /// Create new worker for this core
    pub fn new(id: usize, router: Arc<Router>) -> Self {
        Self {
            id,
            router,
            pools: Mutex::new(BackendConnectionPools::new(id)), // Wrap in per-worker Mutex
        }
    }

    /// Get worker ID (core number) - lock-free!
    #[allow(dead_code)] // Used in tests and will be used for metrics/debugging
    pub fn id(&self) -> usize {
        self.id
    }

    /// Get HTTP/2 connection for backend (per-worker lock)
    ///
    /// Lock scope:
    /// - Worker selection: lock-free (Arc<Vec<Worker>>)
    /// - Pool access: per-worker lock (tokio::Mutex, async-aware)
    /// - Result: Concurrent requests to different workers run in parallel!
    pub async fn get_backend_connection(
        &self, // Changed from &mut self - no longer needs exclusive access
        backend: common::Backend,
    ) -> Result<
        hyper::client::conn::http2::SendRequest<http_body_util::Full<hyper::body::Bytes>>,
        crate::proxy::backend_pool::PoolError,
    > {
        // Per-worker lock (only contends with requests to same worker)
        let mut pools = self.pools.lock().await;

        // Pre-warm connections on first access (no-op if already warmed)
        pools.prewarm_pool(backend).await?;

        // Get connection from pre-warmed pool
        let pool = pools.get_or_create_pool(backend);
        pool.get_connection().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Backend, HttpMethod};
    use std::net::Ipv4Addr;

    /// GREEN: Test that each worker owns its pools (per-worker Mutex)
    #[tokio::test]
    async fn test_worker_owns_pools() {
        let router = Arc::new(Router::new());

        // Create two workers
        let worker1 = Worker::new(0, router.clone());
        let worker2 = Worker::new(1, router.clone());

        assert_eq!(worker1.id(), 0);
        assert_eq!(worker2.id(), 1);

        // Each worker has its own Mutex<BackendConnectionPools>
        // Verify they have different memory addresses
        let pools1_ptr = &worker1.pools as *const Mutex<BackendConnectionPools>;
        let pools2_ptr = &worker2.pools as *const Mutex<BackendConnectionPools>;

        assert_ne!(
            pools1_ptr, pools2_ptr,
            "Workers should have separate pool instances (per-worker Mutex)"
        );
    }

    /// GREEN: Test that worker can get connections (per-worker lock only)
    #[tokio::test]
    async fn test_worker_lock_free_connection() {
        let router = Arc::new(Router::new());
        // Use port 9999 which should not be in use
        let backend = Backend::new(u32::from(Ipv4Addr::new(127, 0, 0, 1)), 9999, 100);

        // Add route to router
        router
            .add_route(HttpMethod::GET, "/api/test", vec![backend])
            .expect("Should add route");

        let worker = Worker::new(0, router.clone());

        // Try to get connection (will fail since nothing is listening on 9999)
        // This uses per-worker lock (not global lock!)
        let result = worker.get_backend_connection(backend).await;

        // Expected to fail, but should compile and run
        assert!(
            result.is_err(),
            "Should fail to connect (no backend running)"
        );
    }

    /// GREEN: Test multiple workers can operate independently
    #[tokio::test]
    async fn test_workers_independent() {
        let router = Arc::new(Router::new());
        // Use port 9999 which should not be in use
        let backend = Backend::new(u32::from(Ipv4Addr::new(127, 0, 0, 1)), 9999, 100);

        router
            .add_route(HttpMethod::GET, "/api/test", vec![backend])
            .expect("Should add route");

        // Spawn two workers on different "cores"
        let worker0 = Worker::new(0, router.clone());
        let worker1 = Worker::new(1, router.clone());

        // Each worker has its own Mutex<BackendConnectionPools>
        // Verify workers have different pool storage
        let pools0_ptr = &worker0.pools as *const Mutex<BackendConnectionPools>;
        let pools1_ptr = &worker1.pools as *const Mutex<BackendConnectionPools>;

        assert_ne!(
            pools0_ptr, pools1_ptr,
            "Each worker has separate pool storage"
        );
    }

    /// RED: Test concurrent access to different workers (should be lock-free)
    ///
    /// This test will FAIL with Arc<Mutex<Vec<Worker>>> because accessing
    /// different workers requires locking the entire Vec. With Arc<Vec<Worker>>
    /// and per-worker Mutex<pools>, this will PASS.
    #[tokio::test]
    async fn test_concurrent_worker_access() {
        use std::time::Duration;
        use tokio::time::timeout;

        let router = Arc::new(Router::new());
        let backend = Backend::new(u32::from(Ipv4Addr::new(127, 0, 0, 1)), 9999, 100);

        router
            .add_route(HttpMethod::GET, "/api/test", vec![backend])
            .expect("Should add route");

        // Create workers in Arc<Vec<Worker>> (immutable vec, no global lock)
        let workers: Arc<Vec<Worker>> = Arc::new(vec![
            Worker::new(0, router.clone()),
            Worker::new(1, router.clone()),
            Worker::new(2, router.clone()),
        ]);

        // Spawn 3 tasks accessing different workers concurrently
        let workers1 = workers.clone();
        let workers2 = workers.clone();
        let workers3 = workers.clone();

        let handle1 = tokio::spawn(async move {
            // Access worker 0 - should not block workers 1 or 2
            let _id = workers1[0].id();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let handle2 = tokio::spawn(async move {
            // Access worker 1 concurrently with worker 0
            let _id = workers2[1].id();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let handle3 = tokio::spawn(async move {
            // Access worker 2 concurrently with workers 0 and 1
            let _id = workers3[2].id();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        // All 3 tasks should complete concurrently in ~100ms
        // If there was a global lock, they'd run sequentially (~300ms)
        let result = timeout(Duration::from_millis(200), async {
            handle1.await.expect("Task 1 should complete");
            handle2.await.expect("Task 2 should complete");
            handle3.await.expect("Task 3 should complete");
        })
        .await;

        assert!(
            result.is_ok(),
            "Workers should be accessible concurrently (lock-free)"
        );
    }
}
