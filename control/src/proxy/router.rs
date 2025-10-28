//! HTTP Router - selects backends using Maglev consistent hashing
//!
//! TDD: Starting with tests, implementing minimal code to pass

use common::{fnv1a_hash, maglev_build_compact_table, maglev_lookup_compact, Backend, HttpMethod};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Route configuration for a path
struct Route {
    backends: Vec<Backend>,
    maglev_table: Vec<u8>,
}

/// Route lookup key
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RouteKey {
    method: HttpMethod,
    path_hash: u64,
}

/// HTTP Router - selects backend for incoming requests
pub struct Router {
    routes: Arc<RwLock<HashMap<RouteKey, Route>>>,
}

impl Router {
    /// Create new router
    pub fn new() -> Self {
        Self {
            routes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add route with backends
    pub fn add_route(
        &self,
        method: HttpMethod,
        path: &str,
        backends: Vec<Backend>,
    ) -> Result<(), String> {
        let path_hash = fnv1a_hash(path.as_bytes());
        let key = RouteKey { method, path_hash };

        // Build Maglev table for this route
        let maglev_table = maglev_build_compact_table(&backends);

        let route = Route {
            backends,
            maglev_table,
        };

        let mut routes = self.routes.write().unwrap();
        routes.insert(key, route);

        Ok(())
    }

    /// Select backend for request
    pub fn select_backend(&self, method: HttpMethod, path: &str) -> Option<Backend> {
        let path_hash = fnv1a_hash(path.as_bytes());
        let key = RouteKey { method, path_hash };

        let routes = self.routes.read().unwrap();
        let route = routes.get(&key)?;

        // Use Maglev to select backend based on path hash
        let backend_idx = maglev_lookup_compact(path_hash, &route.maglev_table);
        route.backends.get(backend_idx as usize).copied()
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

        // Select backend for this request
        let backend = router
            .select_backend(HttpMethod::GET, "/api/users")
            .expect("Should find backend");

        // Should select one of the two backends
        let backend_ip = Ipv4Addr::from(backend.ipv4);
        assert!(
            backend_ip == Ipv4Addr::new(10, 0, 1, 1) || backend_ip == Ipv4Addr::new(10, 0, 1, 2)
        );
        assert_eq!(backend.port, 8080);
    }

    #[test]
    fn test_router_no_route() {
        // GREEN: Router returns None when no routes
        let router = Router::new();

        // No routes added - should return None
        let backend = router.select_backend(HttpMethod::GET, "/api/users");
        assert!(backend.is_none());
    }
}
