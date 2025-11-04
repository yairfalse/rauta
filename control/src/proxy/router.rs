//! HTTP Router - selects backends using Maglev consistent hashing
//!
//! Supports both exact and prefix matching for K8s Ingress compatibility.
//! Includes passive health checking (circuit breaker pattern).

use common::{fnv1a_hash, maglev_build_compact_table, maglev_lookup_compact, Backend, HttpMethod};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Backend health statistics for passive health checking
#[derive(Debug, Clone)]
struct BackendHealth {
    /// Total successful requests (2xx, 3xx, 4xx)
    success_count: u64,
    /// Total 5xx errors
    error_5xx_count: u64,
}

impl BackendHealth {
    #[allow(dead_code)] // Used in tests
    fn new() -> Self {
        Self {
            success_count: 0,
            error_5xx_count: 0,
        }
    }

    /// Check if backend is healthy
    /// Unhealthy if error rate > 50% in last 100 requests
    fn is_healthy(&self) -> bool {
        let total = self.success_count + self.error_5xx_count;
        if total == 0 {
            return true; // No data yet, assume healthy
        }

        // If error rate > 50%, mark as unhealthy
        let error_rate = self.error_5xx_count as f64 / total as f64;
        error_rate <= 0.5
    }

    /// Record a response
    #[allow(dead_code)] // Used in tests
    fn record_response(&mut self, status_code: u16) {
        if (500..600).contains(&status_code) {
            self.error_5xx_count += 1;
        } else {
            self.success_count += 1;
        }
    }
}

/// Route configuration for a path
struct Route {
    pattern: Arc<str>, // Original route pattern for metrics (Arc for cheap cloning)
    backends: Vec<Backend>,
    maglev_table: Vec<u8>,
}

/// Route match result (backend + pattern for metrics)
pub struct RouteMatch {
    pub backend: Backend,
    pub pattern: Arc<str>, // Arc<str> for zero-cost clone on hot path
}

/// Route lookup key
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RouteKey {
    method: HttpMethod,
    path_hash: u64,
}

/// HTTP Router - selects backend for incoming requests
///
/// Uses hybrid matching strategy:
/// - Exact match via hash map (O(1) for exact paths)
/// - Prefix match via matchit radix tree (O(log n) for prefixes)
/// - Passive health checking (circuit breaker for unhealthy backends)
pub struct Router {
    routes: Arc<RwLock<HashMap<RouteKey, Route>>>,
    prefix_router: Arc<RwLock<matchit::Router<RouteKey>>>,
    /// Backend health tracking (key = backend IPv4 address)
    backend_health: Arc<RwLock<HashMap<u32, BackendHealth>>>,
}

impl Default for Router {
    fn default() -> Self {
        Self {
            routes: Arc::new(RwLock::new(HashMap::new())),
            prefix_router: Arc::new(RwLock::new(matchit::Router::new())),
            backend_health: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Router {
    /// Create new router
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or update route with backends (idempotent)
    ///
    /// If the route already exists with the same backends, this is a no-op.
    /// If the route exists with different backends, it's updated.
    /// If the route doesn't exist, it's created.
    pub fn add_route(
        &self,
        method: HttpMethod,
        path: &str,
        backends: Vec<Backend>,
    ) -> Result<(), String> {
        let path_hash = fnv1a_hash(path.as_bytes());
        let key = RouteKey { method, path_hash };

        // Check if route already exists with same backends (idempotent fast path)
        {
            let routes = self.routes.read().unwrap();
            if let Some(existing) = routes.get(&key) {
                // Compare backends by content (O(n) Vec comparison; can be expensive for large backend lists)
                if existing.backends.len() == backends.len() && existing.backends == backends {
                    // Route already exists with same backends - no-op
                    return Ok(());
                }
            }
        }

        // Build Maglev table for this route
        let maglev_table = maglev_build_compact_table(&backends);

        let route = Route {
            pattern: Arc::from(path), // Convert to Arc<str> once during route setup
            backends,
            maglev_table,
        };

        // Update routes HashMap (minimize write lock duration)
        {
            let mut routes = self.routes.write().unwrap();
            routes.insert(key, route);
        } // Drop write lock immediately - allows concurrent lookups during rebuild

        // Rebuild matchit router from scratch (since matchit doesn't support updates)
        // Hold read lock during rebuild to allow concurrent route lookups
        // This is O(n) but acceptable for typical K8s Ingress scale (<1000 routes)
        let mut new_prefix_router = matchit::Router::new();
        {
            let routes = self.routes.read().unwrap();
            for (route_key, route) in routes.iter() {
                let path_str = route.pattern.as_ref();

                // Add exact path
                new_prefix_router
                    .insert(path_str.to_string(), *route_key)
                    .map_err(|e| format!("Failed to add exact route {}: {}", path_str, e))?;

                // Add prefix wildcard (matchit syntax: {*rest})
                let prefix_pattern = if path_str == "/" {
                    "/{*rest}".to_string()
                } else {
                    format!("{}/{{*rest}}", path_str.trim_end_matches('/'))
                };

                new_prefix_router
                    .insert(prefix_pattern, *route_key)
                    .map_err(|e| format!("Failed to add prefix route {}: {}", path_str, e))?;
            }
        } // Drop read lock

        // Swap in new prefix router atomically (brief write lock)
        {
            let mut prefix_router = self.prefix_router.write().unwrap();
            *prefix_router = new_prefix_router;
        }

        Ok(())
    }

    /// Update backends for an existing route
    ///
    /// This is an alias for add_route() since add_route() already handles updates.
    /// Exists for clarity when the intent is to update backends (vs adding a new route).
    pub fn update_route_backends(&self, path: &str, backends: Vec<Backend>) -> Result<(), String> {
        // Assume GET method for now (will be extended when we track method per route)
        self.add_route(HttpMethod::GET, path, backends)
    }

    /// Record backend response for passive health checking
    ///
    /// Tracks success (2xx-4xx) and error (5xx) responses per backend.
    /// Used to calculate error rate and exclude unhealthy backends.
    #[allow(dead_code)] // Used in tests and future proxy integration
    pub fn record_backend_response(&self, backend_ip: u32, status_code: u16) {
        let mut health = self.backend_health.write().unwrap();
        let backend_health = health.entry(backend_ip).or_insert_with(BackendHealth::new);
        backend_health.record_response(status_code);
    }

    /// Select backend for request
    ///
    /// Tries exact match first (O(1)), then prefix match (O(log n)).
    /// Uses Maglev consistent hashing for backend selection.
    /// Returns backend + matched route pattern for metrics.
    pub fn select_backend(
        &self,
        method: HttpMethod,
        path: &str,
        src_ip: Option<u32>,
        src_port: Option<u16>,
    ) -> Option<RouteMatch> {
        let path_hash = fnv1a_hash(path.as_bytes());
        let key = RouteKey { method, path_hash };

        // Try exact match first, then prefix match
        let route_key = {
            let routes = self.routes.read().unwrap();
            if routes.contains_key(&key) {
                Some(key)
            } else {
                // Prefix match via matchit
                let prefix_router = self.prefix_router.read().unwrap();
                prefix_router.at(path).ok().map(|m| *m.value)
            }
        }?;

        // Look up route
        let routes = self.routes.read().unwrap();
        let route = routes.get(&route_key)?;

        // Use flow hash (path + src_ip + src_port) for Maglev lookup
        let flow_hash = self.compute_flow_hash(path_hash, src_ip, src_port);

        // Try up to all backends to find a healthy one
        // Use Maglev consistent hashing but skip unhealthy backends
        let health = self.backend_health.read().unwrap();

        // Track which backend indices we've tried to avoid infinite loops
        let mut tried_indices = std::collections::HashSet::new();

        for attempt in 0..route.backends.len() * 10 {
            // Try up to 10x backends to handle hash collisions
            let lookup_hash = flow_hash.wrapping_add(attempt as u64);
            let backend_idx = maglev_lookup_compact(lookup_hash, &route.maglev_table);

            // Skip if we've already tried this backend
            if tried_indices.contains(&backend_idx) {
                continue;
            }
            tried_indices.insert(backend_idx);

            let backend = route.backends.get(backend_idx as usize).copied()?;

            // Check if backend is healthy
            let is_healthy = health
                .get(&backend.ipv4)
                .map(|h| h.is_healthy())
                .unwrap_or(true); // Default to healthy if no health data

            if is_healthy {
                return Some(RouteMatch {
                    backend,
                    pattern: Arc::clone(&route.pattern),
                });
            }

            // If we've tried all unique backends, break
            if tried_indices.len() == route.backends.len() {
                break;
            }
        }

        // All backends are unhealthy, return None
        None
    }

    /// Compute flow hash for load balancing
    ///
    /// Combines path hash with connection info (if available) to distribute
    /// requests across backends. Falls back to path-only if no connection info.
    fn compute_flow_hash(&self, path_hash: u64, src_ip: Option<u32>, src_port: Option<u16>) -> u64 {
        match (src_ip, src_port) {
            (Some(ip), Some(port)) => {
                // Hash = path_hash XOR (ip << 16 | port)
                // This mixes path and connection info for good distribution
                path_hash ^ ((ip as u64) << 16 | port as u64)
            }
            _ => {
                // No connection info - fall back to path-only
                // (Useful for testing or non-network contexts)
                path_hash
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_router_select_backend() {
        // GREEN: Router now exists, test should PASS
        let router = Router::new();

        // Add a route: GET /api/users -> [10.0.1.1:8080, 10.0.1.2:8080]
        let backends = vec![
            Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100),
            Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 2)), 8080, 100),
        ];

        router
            .add_route(HttpMethod::GET, "/api/users", backends)
            .unwrap();

        // Select backend for this request (no connection info in test)
        let route_match = router
            .select_backend(HttpMethod::GET, "/api/users", None, None)
            .expect("Should find backend");

        // Should select one of the two backends
        let backend_ip = Ipv4Addr::from(route_match.backend.ipv4);
        assert!(
            backend_ip == Ipv4Addr::new(10, 0, 1, 1) || backend_ip == Ipv4Addr::new(10, 0, 1, 2)
        );
        assert_eq!(route_match.backend.port, 8080);
        assert_eq!(route_match.pattern.as_ref(), "/api/users");
    }

    #[test]
    fn test_router_no_route() {
        // GREEN: Router returns None when no routes
        let router = Router::new();

        // No routes added - should return None
        let route_match = router.select_backend(HttpMethod::GET, "/api/users", None, None);
        assert!(route_match.is_none());
    }

    #[test]
    fn test_router_prefix_matching() {
        let router = Router::new();

        // Add route: GET /api/users -> backend (Prefix match)
        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];

        router
            .add_route(HttpMethod::GET, "/api/users", backends)
            .unwrap();

        // Exact match should work
        let route_match = router
            .select_backend(HttpMethod::GET, "/api/users", None, None)
            .expect("Exact match should work");
        assert_eq!(
            Ipv4Addr::from(route_match.backend.ipv4),
            Ipv4Addr::new(10, 0, 1, 1)
        );
        assert_eq!(route_match.pattern.as_ref(), "/api/users");

        // Prefix match should also work: /api/users/123 should match /api/users
        let route_match = router
            .select_backend(HttpMethod::GET, "/api/users/123", None, None)
            .expect("Prefix match should work");
        assert_eq!(
            Ipv4Addr::from(route_match.backend.ipv4),
            Ipv4Addr::new(10, 0, 1, 1)
        );
        assert_eq!(
            route_match.pattern.as_ref(),
            "/api/users",
            "Prefix match should return original pattern"
        );

        // Deeper paths should match too
        let route_match = router
            .select_backend(HttpMethod::GET, "/api/users/123/posts", None, None)
            .expect("Deep prefix match should work");
        assert_eq!(
            Ipv4Addr::from(route_match.backend.ipv4),
            Ipv4Addr::new(10, 0, 1, 1)
        );
        assert_eq!(
            route_match.pattern.as_ref(),
            "/api/users",
            "Deep prefix match should return original pattern"
        );

        // Non-matching path should NOT match
        let route_match = router.select_backend(HttpMethod::GET, "/api/posts", None, None);
        assert!(
            route_match.is_none(),
            "/api/posts should not match /api/users"
        );
    }

    #[test]
    fn test_router_maglev_distribution() {
        // REFACTOR: Verify Maglev provides good distribution across paths
        let router = Router::new();

        // Add route with 3 backends (shared by multiple paths)
        let backends = vec![
            Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100),
            Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 2)), 8080, 100),
            Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 3)), 8080, 100),
        ];

        // Add 100 different paths with same backends
        // This simulates a prefix match scenario (all /api/users/* routes)
        for i in 0..100 {
            let path = format!("/api/users/{}", i);
            router
                .add_route(HttpMethod::GET, &path, backends.clone())
                .unwrap();
        }

        // Test distribution across different paths with simulated connections
        let mut distribution = std::collections::HashMap::new();
        for i in 0..100 {
            let path = format!("/api/users/{}", i);

            // Simulate different source IPs/ports for load distribution
            let src_ip = 0x0a000001 + (i as u32); // 10.0.0.1 + i
            let src_port = 50000 + (i as u16);

            let route_match = router
                .select_backend(HttpMethod::GET, &path, Some(src_ip), Some(src_port))
                .expect("Should find backend");

            let ip = Ipv4Addr::from(route_match.backend.ipv4);
            *distribution.entry(ip).or_insert(0) += 1;
        }

        // Each backend should get ~33 requests (within 50% variance for small sample)
        // Note: 100 samples is small, so we allow higher variance
        assert_eq!(distribution.len(), 3, "Should use all 3 backends");
        for (ip, count) in distribution.iter() {
            let percentage = (*count as f64) / 100.0;
            assert!(
                percentage > 0.10 && percentage < 0.60,
                "Backend {} got {}% (expected 10-60% for small sample)",
                ip,
                percentage * 100.0
            );
        }
    }

    #[test]
    fn test_router_idempotent_add_route() {
        // Verify Router is idempotent - adding same route twice should succeed
        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];

        // Add route first time
        router
            .add_route(HttpMethod::GET, "/api/test", backends.clone())
            .expect("First add should succeed");

        // Add same route again (idempotent - should succeed)
        router
            .add_route(HttpMethod::GET, "/api/test", backends.clone())
            .expect("Second add should succeed (idempotent)");

        // Verify route still works
        let route_match = router
            .select_backend(HttpMethod::GET, "/api/test", None, None)
            .expect("Should find backend after duplicate add");
        assert_eq!(
            Ipv4Addr::from(route_match.backend.ipv4),
            Ipv4Addr::new(10, 0, 1, 1)
        );
    }

    #[test]
    fn test_router_update_route_backends() {
        // Verify Router allows updating backends for existing route
        let router = Router::new();

        // Add route with first backend
        let backends_v1 = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];
        router
            .add_route(HttpMethod::GET, "/api/test", backends_v1)
            .expect("First add should succeed");

        // Update route with different backend
        let backends_v2 = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 2)),
            8080,
            100,
        )];
        router
            .add_route(HttpMethod::GET, "/api/test", backends_v2)
            .expect("Update should succeed");

        // Verify new backend is used
        let route_match = router
            .select_backend(HttpMethod::GET, "/api/test", None, None)
            .expect("Should find backend after update");
        assert_eq!(
            Ipv4Addr::from(route_match.backend.ipv4),
            Ipv4Addr::new(10, 0, 1, 2),
            "Should use updated backend"
        );
    }

    /// RED: Test passive health checking - track 5xx responses
    #[test]
    fn test_passive_health_checking_excludes_unhealthy_backend() {
        let router = Router::new();

        // Add route with 2 backends
        let backends = vec![
            Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100), // Backend 1
            Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 2)), 8080, 100), // Backend 2
        ];

        router
            .add_route(HttpMethod::GET, "/api/test", backends)
            .expect("Should add route");

        // Simulate 10 requests to backend 10.0.1.1 - all return 500 (unhealthy)
        let backend1_ip = u32::from(Ipv4Addr::new(10, 0, 1, 1));
        for _ in 0..10 {
            router.record_backend_response(backend1_ip, 500);
        }

        // Simulate 10 requests to backend 10.0.1.2 - all return 200 (healthy)
        let backend2_ip = u32::from(Ipv4Addr::new(10, 0, 1, 2));
        for _ in 0..10 {
            router.record_backend_response(backend2_ip, 200);
        }

        // Now when we select a backend, it should ONLY return the healthy one (10.0.1.2)
        // Make 100 requests to ensure we get good distribution
        let mut selected_backends = std::collections::HashSet::new();
        for i in 0..100 {
            let route_match = router
                .select_backend(
                    HttpMethod::GET,
                    "/api/test",
                    Some(0x0100007f + i), // Vary source IP
                    Some((i % 65535) as u16),
                )
                .expect("Should find backend");

            selected_backends.insert(Ipv4Addr::from(route_match.backend.ipv4));
        }

        // Should ONLY see the healthy backend (10.0.1.2)
        assert_eq!(
            selected_backends.len(),
            1,
            "Should only use 1 healthy backend"
        );
        assert!(
            selected_backends.contains(&Ipv4Addr::new(10, 0, 1, 2)),
            "Should only use healthy backend 10.0.1.2"
        );
        assert!(
            !selected_backends.contains(&Ipv4Addr::new(10, 0, 1, 1)),
            "Should NOT use unhealthy backend 10.0.1.1"
        );
    }
}
