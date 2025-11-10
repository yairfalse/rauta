//! HTTP Router - selects backends using Maglev consistent hashing
//!
//! Supports both exact and prefix matching for K8s Ingress compatibility.
//! Includes passive health checking (circuit breaker) and connection draining (graceful removal).

use common::{fnv1a_hash, maglev_build_compact_table, maglev_lookup_compact, Backend, HttpMethod};
use regex::Regex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Backend draining state (for graceful removal)
#[derive(Debug, Clone)]
struct BackendDraining {
    /// When this backend should be force-removed
    #[allow(dead_code)] // Used in cleanup_expired_draining_backends
    deadline: Instant,
}

impl BackendDraining {
    #[allow(dead_code)] // Used in drain_backend()
    fn new(drain_timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + drain_timeout,
        }
    }

    #[allow(dead_code)] // Will be used in future EndpointSlice integration
    fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

/// Backend health statistics for passive health checking
#[derive(Debug, Clone)]
struct BackendHealth {
    /// Sliding window of last 100 request results: true = 5xx error, false = success
    window: VecDeque<bool>,
}

const WINDOW_SIZE: usize = 100;

impl BackendHealth {
    #[allow(dead_code)] // Used in tests
    fn new() -> Self {
        Self {
            window: VecDeque::with_capacity(WINDOW_SIZE),
        }
    }

    /// Check if backend is healthy
    /// Unhealthy if error rate > 50% in last 100 requests
    fn is_healthy(&self) -> bool {
        let total = self.window.len();
        if total == 0 {
            return true; // No data yet, assume healthy
        }

        // If error rate > 50%, mark as unhealthy
        let error_count = self.window.iter().filter(|&&is_error| is_error).count();
        let error_rate = error_count as f64 / total as f64;
        error_rate <= 0.5
    }

    /// Record a response
    #[allow(dead_code)] // Used in tests
    fn record_response(&mut self, status_code: u16) {
        let is_error = (500..600).contains(&status_code);
        if self.window.len() == WINDOW_SIZE {
            self.window.pop_front();
        }
        self.window.push_back(is_error);
    }
}

/// Route configuration for a path
struct Route {
    pattern: Arc<str>, // Original route pattern for metrics (Arc for cheap cloning)
    backends: Vec<Backend>,
    maglev_table: Vec<u8>,
    header_matches: Vec<HeaderMatch>, // Gateway API header matching (empty = no header constraints)
    #[allow(dead_code)] // Used in GREEN phase
    method_matches: Option<Vec<HttpMethod>>, // Gateway API method matching (None = match all methods)
    #[allow(dead_code)] // Used in GREEN phase for Feature 3
    query_param_matches: Vec<QueryParamMatch>, // Gateway API query parameter matching (empty = no constraints)
    #[allow(dead_code)] // Used in GREEN phase for Feature 4
    request_filters: Option<crate::proxy::filters::RequestHeaderModifier>, // Gateway API request header filters
}

/// Route match result (backend + pattern for metrics)
pub struct RouteMatch {
    pub backend: Backend,
    pub pattern: Arc<str>, // Arc<str> for zero-cost clone on hot path
    #[allow(dead_code)] // Used in server.rs to apply filters during proxying
    pub request_filters: Option<crate::proxy::filters::RequestHeaderModifier>,
}

/// Header match type for Gateway API conformance
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Used in tests during TDD implementation
pub enum HeaderMatchType {
    /// Exact match (default)
    Exact,
    /// Regular expression match
    RegularExpression,
}

/// Header match configuration for routes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderMatch {
    /// Header name (case-insensitive per RFC 7230)
    pub name: String,
    /// Header value to match
    pub value: String,
    /// Match type (exact or regex)
    pub match_type: HeaderMatchType,
}

/// Query parameter match type for Gateway API conformance
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Used in tests during TDD implementation
pub enum QueryParamMatchType {
    /// Exact match (default)
    Exact,
    /// Regular expression match
    RegularExpression,
}

/// Query parameter match configuration for routes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryParamMatch {
    /// Query parameter name
    pub name: String,
    /// Query parameter value to match
    pub value: String,
    /// Match type (exact or regex)
    pub match_type: QueryParamMatchType,
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
    /// Backends marked for draining (graceful removal)
    draining_backends: Arc<RwLock<HashMap<u32, BackendDraining>>>,
    /// Backend health tracking (key = backend IPv4 address)
    backend_health: Arc<RwLock<HashMap<u32, BackendHealth>>>,
}

impl Default for Router {
    fn default() -> Self {
        Self {
            routes: Arc::new(RwLock::new(HashMap::new())),
            prefix_router: Arc::new(RwLock::new(matchit::Router::new())),
            draining_backends: Arc::new(RwLock::new(HashMap::new())),
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
            header_matches: Vec::new(), // No header matching for basic routes
            method_matches: None,       // No method constraints (match all methods)
            query_param_matches: Vec::new(), // No query param constraints
            request_filters: None,      // No request header filters
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
    pub fn update_route_backends(
        &self,
        method: HttpMethod,
        path: &str,
        backends: Vec<Backend>,
    ) -> Result<(), String> {
        // Update backends for the specified HTTP method and path
        self.add_route(method, path, backends)
    }

    /// Mark backend for draining (graceful removal)
    ///
    /// When EndpointSlice removes a backend, don't immediately remove it.
    /// Instead mark as draining to allow existing connections to finish.
    /// After drain_timeout, the backend will be excluded from routing.
    ///
    /// This implements Google's Maglev pattern: "rolling restart of machines
    /// to upgrade Maglevs in a cluster, draining traffic from each one a few
    /// moments beforehand"
    #[allow(dead_code)] // Used in tests and future EndpointSlice integration
    pub fn drain_backend(&self, backend_ip: u32, drain_timeout: Duration) {
        let mut draining = self.draining_backends.write().unwrap();
        draining.insert(backend_ip, BackendDraining::new(drain_timeout));
    }

    /// Check if backend is marked for draining
    #[allow(dead_code)] // Used in tests
    pub fn is_backend_draining(&self, backend_ip: u32) -> bool {
        let draining = self.draining_backends.read().unwrap();
        draining.contains_key(&backend_ip)
    }

    /// Clean up expired draining backends
    #[allow(dead_code)] // Will be used in EndpointSlice watcher integration
    fn cleanup_expired_draining_backends(&self) {
        let mut draining = self.draining_backends.write().unwrap();
        draining.retain(|_ip, state| !state.is_expired());
    }

    /// Record backend response for passive health checking
    ///
    /// Tracks success (2xx-4xx) and error (5xx) responses per backend.
    /// Used to calculate error rate and exclude unhealthy backends.
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

        // GREEN phase: Check method constraints (Gateway API)
        if let Some(method_matches) = &route.method_matches {
            // Route has method constraints - check if request method matches
            if !method_matches.contains(&method) {
                return None; // Method doesn't match
            }
        }
        // If method_matches is None, route accepts all methods (default behavior)

        // Use flow hash (path + src_ip + src_port) for Maglev lookup
        let flow_hash = self.compute_flow_hash(path_hash, src_ip, src_port);

        // Try up to all backends to find one that is BOTH healthy AND not draining
        let health = self.backend_health.read().unwrap();
        let draining = self.draining_backends.read().unwrap();

        // Track which backend indices we've tried to avoid infinite loops
        let mut tried_indices = HashSet::new();

        for attempt in 0..route.backends.len() * 10 {
            let lookup_hash = flow_hash.wrapping_add(attempt as u64);
            let backend_idx = maglev_lookup_compact(lookup_hash, &route.maglev_table);

            // Skip if we've already tried this backend
            if tried_indices.contains(&backend_idx) {
                continue;
            }
            tried_indices.insert(backend_idx);

            let backend = route.backends.get(backend_idx as usize).copied()?;

            // Check if backend is healthy (passive health checking)
            let is_healthy = health
                .get(&backend.ipv4)
                .map(|h| h.is_healthy())
                .unwrap_or(true); // Default to healthy if no health data

            // Check if backend is draining (connection draining)
            let is_draining = draining.contains_key(&backend.ipv4);

            // Skip if unhealthy OR draining
            if !is_healthy || is_draining {
                if tried_indices.len() == route.backends.len() {
                    break; // All backends tried
                }
                continue;
            }

            // Found a healthy, non-draining backend
            return Some(RouteMatch {
                backend,
                pattern: Arc::clone(&route.pattern),
                request_filters: route.request_filters.clone(),
            });
        }

        // All backends are either unhealthy or draining
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

    /// Add route with header matching support (Gateway API conformance)
    #[allow(dead_code)] // Used in tests
    pub fn add_route_with_headers(
        &self,
        method: HttpMethod,
        path: &str,
        backends: Vec<Backend>,
        header_matches: Vec<HeaderMatch>,
    ) -> Result<(), String> {
        // GREEN phase: Store header_matches and create route
        let path_hash = fnv1a_hash(path.as_bytes());
        let key = RouteKey { method, path_hash };

        // Build Maglev table
        let maglev_table = maglev_build_compact_table(&backends);

        let route = Route {
            pattern: Arc::from(path),
            backends,
            maglev_table,
            header_matches,                  // Store header matches for validation
            method_matches: None,            // No method constraints by default
            query_param_matches: Vec::new(), // No query param constraints
            request_filters: None,           // No request header filters
        };

        // Update routes HashMap
        {
            let mut routes = self.routes.write().unwrap();
            routes.insert(key, route);
        }

        // Rebuild matchit router (same as add_route)
        let mut new_prefix_router = matchit::Router::new();
        {
            let routes = self.routes.read().unwrap();
            for (route_key, route) in routes.iter() {
                let path_str = route.pattern.as_ref();

                new_prefix_router
                    .insert(path_str.to_string(), *route_key)
                    .map_err(|e| format!("Failed to add exact route {}: {}", path_str, e))?;

                let prefix_pattern = if path_str == "/" {
                    "/{*rest}".to_string()
                } else {
                    format!("{}/{{*rest}}", path_str.trim_end_matches('/'))
                };

                new_prefix_router
                    .insert(prefix_pattern, *route_key)
                    .map_err(|e| format!("Failed to add prefix route {}: {}", path_str, e))?;
            }
        }

        {
            let mut prefix_router = self.prefix_router.write().unwrap();
            *prefix_router = new_prefix_router;
        }

        Ok(())
    }

    /// Add route with method matching support (Gateway API conformance)
    #[allow(dead_code)] // Used in tests
    pub fn add_route_with_methods(
        &self,
        method: HttpMethod,
        path: &str,
        backends: Vec<Backend>,
        method_matches: Vec<HttpMethod>,
    ) -> Result<(), String> {
        // GREEN phase: Store method constraints in route
        let path_hash = fnv1a_hash(path.as_bytes());
        let key = RouteKey { method, path_hash };

        // Build Maglev table
        let maglev_table = maglev_build_compact_table(&backends);

        let route = Route {
            pattern: Arc::from(path),
            backends,
            maglev_table,
            header_matches: Vec::new(),           // No header constraints
            method_matches: Some(method_matches), // Store method constraints
            query_param_matches: Vec::new(),      // No query param constraints
            request_filters: None,                // No request header filters
        };

        // Update routes HashMap
        {
            let mut routes = self.routes.write().unwrap();
            routes.insert(key, route);
        }

        // Rebuild matchit router (same as add_route)
        let mut new_prefix_router = matchit::Router::new();
        {
            let routes = self.routes.read().unwrap();
            for (route_key, route) in routes.iter() {
                let path_str = route.pattern.as_ref();

                new_prefix_router
                    .insert(path_str.to_string(), *route_key)
                    .map_err(|e| format!("Failed to add exact route {}: {}", path_str, e))?;

                let prefix_pattern = if path_str == "/" {
                    "/{*rest}".to_string()
                } else {
                    format!("{}/{{*rest}}", path_str.trim_end_matches('/'))
                };

                new_prefix_router
                    .insert(prefix_pattern, *route_key)
                    .map_err(|e| format!("Failed to add prefix route {}: {}", path_str, e))?;
            }
        }

        {
            let mut prefix_router = self.prefix_router.write().unwrap();
            *prefix_router = new_prefix_router;
        }

        Ok(())
    }

    /// Add route with query parameter matching support (Gateway API conformance)
    #[allow(dead_code)] // Used in tests
    pub fn add_route_with_query_params(
        &self,
        method: HttpMethod,
        path: &str,
        backends: Vec<Backend>,
        query_param_matches: Vec<QueryParamMatch>,
    ) -> Result<(), String> {
        // GREEN phase: Store query param matches in route
        let path_hash = fnv1a_hash(path.as_bytes());
        let key = RouteKey { method, path_hash };

        // Build Maglev table
        let maglev_table = maglev_build_compact_table(&backends);

        let route = Route {
            pattern: Arc::from(path),
            backends,
            maglev_table,
            header_matches: Vec::new(), // No header constraints
            method_matches: None,       // No method constraints
            query_param_matches,        // Store query param constraints
            request_filters: None,      // No request header filters
        };

        // Update routes HashMap
        {
            let mut routes = self.routes.write().unwrap();
            routes.insert(key, route);
        }

        // Rebuild matchit router (same as add_route)
        let mut new_prefix_router = matchit::Router::new();
        {
            let routes = self.routes.read().unwrap();
            for (route_key, route) in routes.iter() {
                let path_str = route.pattern.as_ref();

                new_prefix_router
                    .insert(path_str.to_string(), *route_key)
                    .map_err(|e| format!("Failed to add exact route {}: {}", path_str, e))?;

                let prefix_pattern = if path_str == "/" {
                    "/{*rest}".to_string()
                } else {
                    format!("{}/{{*rest}}", path_str.trim_end_matches('/'))
                };

                new_prefix_router
                    .insert(prefix_pattern, *route_key)
                    .map_err(|e| format!("Failed to add prefix route {}: {}", path_str, e))?;
            }
        }

        {
            let mut prefix_router = self.prefix_router.write().unwrap();
            *prefix_router = new_prefix_router;
        }

        Ok(())
    }

    /// Add route with request header filters (Gateway API HTTPRouteBackendRequestHeaderModification)
    /// Filters are applied in server.rs before proxying to backend
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn add_route_with_filters(
        &self,
        method: HttpMethod,
        path: &str,
        backends: Vec<Backend>,
        filters: crate::proxy::filters::RequestHeaderModifier,
    ) -> Result<(), String> {
        let path_hash = fnv1a_hash(path.as_bytes());
        let key = RouteKey { method, path_hash };

        // Build Maglev table
        let maglev_table = maglev_build_compact_table(&backends);

        let route = Route {
            pattern: Arc::from(path),
            backends,
            maglev_table,
            header_matches: Vec::new(),      // No header constraints
            method_matches: None,            // No method constraints
            query_param_matches: Vec::new(), // No query param constraints
            request_filters: Some(filters),  // Store filters for server.rs to apply
        };

        // Update routes HashMap
        {
            let mut routes = self.routes.write().unwrap();
            routes.insert(key, route);
        }

        // Rebuild matchit router from scratch (since matchit doesn't support updates)
        let routes = self.routes.read().unwrap();
        let mut new_prefix_router = matchit::Router::new();

        for (route_key, route) in routes.iter() {
            let path_str = route.pattern.as_ref();

            // Add exact match route
            new_prefix_router
                .insert(path_str.to_string(), *route_key)
                .map_err(|e| format!("Failed to add route {}: {}", path_str, e))?;

            // Add prefix match route (/{*rest} pattern)
            let prefix_pattern = if path_str == "/" {
                "/{*rest}".to_string()
            } else {
                format!("{}/{{*rest}}", path_str.trim_end_matches('/'))
            };

            new_prefix_router
                .insert(prefix_pattern, *route_key)
                .map_err(|e| format!("Failed to add prefix route {}: {}", path_str, e))?;
        }

        {
            let mut prefix_router = self.prefix_router.write().unwrap();
            *prefix_router = new_prefix_router;
        }

        Ok(())
    }

    /// Select backend with header matching support (Gateway API conformance)
    ///
    /// Validates request headers against route's header_matches before selecting backend.
    /// Supports Exact and RegularExpression matching per Gateway API spec.
    /// Header names are case-insensitive per RFC 7230.
    #[allow(dead_code)] // Used in tests
    pub fn select_backend_with_headers(
        &self,
        method: HttpMethod,
        path: &str,
        request_headers: Vec<(&str, &str)>,
        src_ip: Option<u32>,
        src_port: Option<u16>,
    ) -> Option<RouteMatch> {
        // GREEN phase: Full header matching implementation
        let path_hash = fnv1a_hash(path.as_bytes());
        let key = RouteKey { method, path_hash };

        // Try exact match first, then prefix match (same as select_backend)
        let route_key = {
            let routes = self.routes.read().unwrap();
            if routes.contains_key(&key) {
                Some(key)
            } else {
                let prefix_router = self.prefix_router.read().unwrap();
                prefix_router.at(path).ok().map(|m| *m.value)
            }
        }?;

        // Look up route
        let routes = self.routes.read().unwrap();
        let route = routes.get(&route_key)?;

        // Check header matches (ALL must match - AND logic)
        if !route.header_matches.is_empty()
            && !headers_match(&route.header_matches, &request_headers)
        {
            return None; // Headers don't match
        }

        // Headers match (or no header constraints) - select backend using Maglev
        let flow_hash = self.compute_flow_hash(path_hash, src_ip, src_port);

        let health = self.backend_health.read().unwrap();
        let draining = self.draining_backends.read().unwrap();
        let mut tried_indices = HashSet::new();

        for attempt in 0..route.backends.len() * 10 {
            let lookup_hash = flow_hash.wrapping_add(attempt as u64);
            let backend_idx = maglev_lookup_compact(lookup_hash, &route.maglev_table);

            if tried_indices.contains(&backend_idx) {
                continue;
            }
            tried_indices.insert(backend_idx);

            let backend = route.backends.get(backend_idx as usize).copied()?;

            let is_healthy = health
                .get(&backend.ipv4)
                .map(|h| h.is_healthy())
                .unwrap_or(true);

            let is_draining = draining.contains_key(&backend.ipv4);

            if !is_healthy || is_draining {
                if tried_indices.len() == route.backends.len() {
                    break;
                }
                continue;
            }

            return Some(RouteMatch {
                backend,
                pattern: Arc::clone(&route.pattern),
                request_filters: route.request_filters.clone(),
            });
        }

        None
    }

    /// Select backend with query parameter matching support (Gateway API conformance)
    ///
    /// Validates request query params against route's query_param_matches before selecting backend.
    /// Supports Exact and RegularExpression matching per Gateway API spec.
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn select_backend_with_query_params(
        &self,
        method: HttpMethod,
        path: &str,
        query_params: Vec<(&str, &str)>,
        src_ip: Option<u32>,
        src_port: Option<u16>,
    ) -> Option<RouteMatch> {
        // GREEN phase: Full query parameter matching implementation
        let path_hash = fnv1a_hash(path.as_bytes());
        let key = RouteKey { method, path_hash };

        // Try exact match first, then prefix match (same as select_backend)
        let route_key = {
            let routes = self.routes.read().unwrap();
            if routes.contains_key(&key) {
                Some(key)
            } else {
                let prefix_router = self.prefix_router.read().unwrap();
                prefix_router.at(path).ok().map(|m| *m.value)
            }
        }?;

        // Look up route
        let routes = self.routes.read().unwrap();
        let route = routes.get(&route_key)?;

        // Check query parameter matches (ALL must match - AND logic)
        if !route.query_param_matches.is_empty()
            && !query_params_match(&route.query_param_matches, &query_params)
        {
            return None; // Query params don't match
        }

        // Query params match (or no query param constraints) - select backend using Maglev
        let flow_hash = self.compute_flow_hash(path_hash, src_ip, src_port);

        let health = self.backend_health.read().unwrap();
        let draining = self.draining_backends.read().unwrap();
        let mut tried_indices = HashSet::new();

        for attempt in 0..route.backends.len() * 10 {
            let lookup_hash = flow_hash.wrapping_add(attempt as u64);
            let backend_idx = maglev_lookup_compact(lookup_hash, &route.maglev_table);

            if tried_indices.contains(&backend_idx) {
                continue;
            }
            tried_indices.insert(backend_idx);

            let backend = route.backends.get(backend_idx as usize).copied()?;

            let is_healthy = health
                .get(&backend.ipv4)
                .map(|h| h.is_healthy())
                .unwrap_or(true);

            let is_draining = draining.contains_key(&backend.ipv4);

            if !is_healthy || is_draining {
                if tried_indices.len() == route.backends.len() {
                    break;
                }
                continue;
            }

            return Some(RouteMatch {
                backend,
                pattern: Arc::clone(&route.pattern),
                request_filters: route.request_filters.clone(),
            });
        }

        None
    }
}

/// Check if request headers match route's header_matches (Gateway API conformance)
///
/// ALL header_matches must be satisfied (AND logic).
/// Header names are case-insensitive per RFC 7230.
fn headers_match(header_matches: &[HeaderMatch], request_headers: &[(&str, &str)]) -> bool {
    for header_match in header_matches {
        // Find matching header in request (case-insensitive name comparison)
        let found = request_headers.iter().any(|(req_name, req_value)| {
            // Case-insensitive header name comparison (RFC 7230)
            if !req_name.eq_ignore_ascii_case(&header_match.name) {
                return false;
            }

            // Check value based on match type
            match header_match.match_type {
                HeaderMatchType::Exact => *req_value == header_match.value,
                HeaderMatchType::RegularExpression => {
                    // Compile regex and check match
                    // Note: In production, we'd cache compiled regexes
                    if let Ok(regex) = Regex::new(&header_match.value) {
                        regex.is_match(req_value)
                    } else {
                        false // Invalid regex = no match
                    }
                }
            }
        });

        if !found {
            return false; // At least one header_match not satisfied
        }
    }

    true // All header_matches satisfied
}

/// Check if request query params match route's query_param_matches (Gateway API conformance)
///
/// ALL query_param_matches must be satisfied (AND logic).
/// Query parameter names are case-sensitive.
fn query_params_match(
    query_param_matches: &[QueryParamMatch],
    request_query_params: &[(&str, &str)],
) -> bool {
    for query_param_match in query_param_matches {
        // Find matching query param in request (case-sensitive name comparison)
        let found = request_query_params.iter().any(|(req_name, req_value)| {
            // Exact name match (query params are case-sensitive)
            if *req_name != query_param_match.name {
                return false;
            }

            // Check value based on match type
            match query_param_match.match_type {
                QueryParamMatchType::Exact => *req_value == query_param_match.value,
                QueryParamMatchType::RegularExpression => {
                    // Compile regex and check match
                    // Note: In production, we'd cache compiled regexes
                    if let Ok(regex) = Regex::new(&query_param_match.value) {
                        regex.is_match(req_value)
                    } else {
                        false // Invalid regex = no match
                    }
                }
            }
        });

        if !found {
            return false; // At least one query_param_match not satisfied
        }
    }

    true // All query_param_matches satisfied
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

    /// Test passive health checking - track 5xx responses
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

    #[test]
    fn test_router_connection_draining() {
        // Test graceful backend removal (Google Maglev pattern)
        // When EndpointSlice removes a backend, don't immediately remove it.
        // Instead: mark as draining for 30s to allow existing connections to finish.
        use std::time::Duration;

        let router = Router::new();

        // Add route with 2 backends
        let backends = vec![
            Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100),
            Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 2)), 8080, 100),
        ];
        router
            .add_route(HttpMethod::GET, "/api/users", backends)
            .expect("Add route should succeed");

        // Mark first backend for draining (30 second grace period)
        let backend_to_drain = u32::from(Ipv4Addr::new(10, 0, 1, 1));
        router.drain_backend(backend_to_drain, Duration::from_secs(30));

        // NEW connections should NOT go to draining backend
        // Try 100 times to ensure we don't get the draining backend
        for i in 0..100 {
            let src_ip = Some(0x0a000001 + i); // Vary IP to test distribution
            let src_port = Some((50000 + i) as u16);

            let route_match = router
                .select_backend(HttpMethod::GET, "/api/users", src_ip, src_port)
                .expect("Should find backend");

            assert_ne!(
                route_match.backend.ipv4, backend_to_drain,
                "New connections should NOT go to draining backend (attempt {})",
                i
            );
            assert_eq!(
                Ipv4Addr::from(route_match.backend.ipv4),
                Ipv4Addr::new(10, 0, 1, 2),
                "Should route to healthy backend only"
            );
        }

        // Verify draining backend is still in routes (not removed yet)
        // This allows existing connections to finish gracefully
        assert!(
            router.is_backend_draining(backend_to_drain),
            "Backend should be marked as draining"
        );
    }

    #[test]
    fn test_router_weighted_backends_canary_deployment() {
        // Test weighted backends for canary deployments
        // Use case: 90% traffic to stable, 10% to canary
        // Maglev supports this by repeating backends in permutation based on weight

        let router = Router::new();

        // 90% stable, 10% canary
        let stable_backend = Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 90);
        let canary_backend = Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 2)), 8080, 10);

        let backends = vec![stable_backend, canary_backend];

        router
            .add_route(HttpMethod::GET, "/api/users", backends)
            .expect("Add route should succeed");

        // Test distribution over 1000 requests
        let mut distribution = std::collections::HashMap::new();
        for i in 0..1000 {
            let src_ip = Some(0x0a000001 + i);
            let src_port = Some((50000 + (i % 60000)) as u16);

            let route_match = router
                .select_backend(HttpMethod::GET, "/api/users", src_ip, src_port)
                .expect("Should find backend");

            *distribution.entry(route_match.backend.ipv4).or_insert(0) += 1;
        }

        // Check distribution matches weights (within 10% variance for 1000 samples)
        let stable_count = distribution.get(&stable_backend.ipv4).copied().unwrap_or(0);
        let canary_count = distribution.get(&canary_backend.ipv4).copied().unwrap_or(0);

        let stable_pct = (stable_count as f64) / 1000.0;
        let canary_pct = (canary_count as f64) / 1000.0;

        assert!(
            (0.80..=1.00).contains(&stable_pct),
            "Stable should get 80-100% of traffic (got {:.1}%)",
            stable_pct * 100.0
        );
        assert!(
            (0.00..=0.20).contains(&canary_pct),
            "Canary should get 0-20% of traffic (got {:.1}%)",
            canary_pct * 100.0
        );

        println!(
            "Weighted distribution: stable={:.1}%, canary={:.1}%",
            stable_pct * 100.0,
            canary_pct * 100.0
        );
    }

    // =============================================================================
    // Gateway API Conformance Tests - Feature 1: HTTPRoute Header Matching (Core)
    // =============================================================================
    // These tests follow TDD approach: RED → GREEN → REFACTOR
    // Currently in RED phase - tests will FAIL until implementation is complete

    #[test]
    fn test_header_match_exact() {
        // RED: Test exact header matching
        // HTTPRoute spec: headers.type = "Exact" (default)
        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];

        // Add route with header match: X-Version = "v1"
        let header_matches = vec![HeaderMatch {
            name: "X-Version".to_string(),
            value: "v1".to_string(),
            match_type: HeaderMatchType::Exact,
        }];

        router
            .add_route_with_headers(HttpMethod::GET, "/api/test", backends, header_matches)
            .expect("Should add route with headers");

        // Request WITH matching header should succeed
        let headers = vec![("X-Version", "v1")];
        let route_match = router
            .select_backend_with_headers(HttpMethod::GET, "/api/test", headers, None, None)
            .expect("Should match with correct header");

        assert_eq!(
            Ipv4Addr::from(route_match.backend.ipv4),
            Ipv4Addr::new(10, 0, 1, 1)
        );

        // Request WITHOUT matching header should fail
        let headers = vec![("X-Version", "v2")];
        let route_match =
            router.select_backend_with_headers(HttpMethod::GET, "/api/test", headers, None, None);

        assert!(
            route_match.is_none(),
            "Should NOT match with incorrect header value"
        );
    }

    #[test]
    fn test_header_match_regex() {
        // RED: Test regex header matching
        // HTTPRoute spec: headers.type = "RegularExpression"
        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 2)),
            8080,
            100,
        )];

        // Add route with regex header match: X-Version matches "v[0-9]+"
        let header_matches = vec![HeaderMatch {
            name: "X-Version".to_string(),
            value: "v[0-9]+".to_string(),
            match_type: HeaderMatchType::RegularExpression,
        }];

        router
            .add_route_with_headers(HttpMethod::GET, "/api/test", backends, header_matches)
            .expect("Should add route with regex headers");

        // Request WITH matching header (v1, v2, v99) should succeed
        for version in &["v1", "v2", "v99"] {
            let headers = vec![("X-Version", *version)];
            let route_match = router
                .select_backend_with_headers(HttpMethod::GET, "/api/test", headers, None, None)
                .unwrap_or_else(|| panic!("Should match header {}", version));

            assert_eq!(
                Ipv4Addr::from(route_match.backend.ipv4),
                Ipv4Addr::new(10, 0, 1, 2)
            );
        }

        // Request WITHOUT matching header should fail
        let headers = vec![("X-Version", "beta")];
        let route_match =
            router.select_backend_with_headers(HttpMethod::GET, "/api/test", headers, None, None);

        assert!(
            route_match.is_none(),
            "Should NOT match with non-matching regex"
        );
    }

    #[test]
    fn test_header_match_multiple_headers() {
        // RED: Test multiple header matches (AND logic)
        // HTTPRoute spec: ALL headers must match
        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 3)),
            8080,
            100,
        )];

        // Add route with multiple header matches
        let header_matches = vec![
            HeaderMatch {
                name: "X-Version".to_string(),
                value: "v1".to_string(),
                match_type: HeaderMatchType::Exact,
            },
            HeaderMatch {
                name: "X-Environment".to_string(),
                value: "production".to_string(),
                match_type: HeaderMatchType::Exact,
            },
        ];

        router
            .add_route_with_headers(HttpMethod::GET, "/api/test", backends, header_matches)
            .expect("Should add route with multiple headers");

        // Request WITH ALL matching headers should succeed
        let headers = vec![("X-Version", "v1"), ("X-Environment", "production")];
        let route_match = router
            .select_backend_with_headers(HttpMethod::GET, "/api/test", headers, None, None)
            .expect("Should match with all headers");

        assert_eq!(
            Ipv4Addr::from(route_match.backend.ipv4),
            Ipv4Addr::new(10, 0, 1, 3)
        );

        // Request WITH ONLY ONE matching header should fail
        let headers = vec![("X-Version", "v1")]; // Missing X-Environment
        let route_match =
            router.select_backend_with_headers(HttpMethod::GET, "/api/test", headers, None, None);

        assert!(
            route_match.is_none(),
            "Should NOT match when missing required headers"
        );
    }

    #[test]
    fn test_header_match_case_insensitive_name() {
        // RED: Test case-insensitive header name matching
        // HTTPRoute spec: Header names are case-insensitive (RFC 7230)
        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 4)),
            8080,
            100,
        )];

        // Add route with header match: x-version = "v1" (lowercase)
        let header_matches = vec![HeaderMatch {
            name: "x-version".to_string(),
            value: "v1".to_string(),
            match_type: HeaderMatchType::Exact,
        }];

        router
            .add_route_with_headers(HttpMethod::GET, "/api/test", backends, header_matches)
            .expect("Should add route");

        // Request with X-Version (uppercase) should match
        let headers = vec![("X-Version", "v1")];
        let route_match = router
            .select_backend_with_headers(HttpMethod::GET, "/api/test", headers, None, None)
            .expect("Should match with case-insensitive header name");

        assert_eq!(
            Ipv4Addr::from(route_match.backend.ipv4),
            Ipv4Addr::new(10, 0, 1, 4)
        );
    }

    // ============================================
    // Feature 2: HTTPRoute Method Matching (Extended)
    // ============================================

    #[test]
    fn test_method_match_single_method() {
        // RED: Test matching a single HTTP method
        // HTTPRoute spec: matches[].method field
        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 2, 1)),
            8080,
            100,
        )];

        // Add route that ONLY matches POST requests
        let method_matches = vec![HttpMethod::POST];
        router
            .add_route_with_methods(HttpMethod::ALL, "/api/test", backends, method_matches)
            .expect("Should add route with method constraint");

        // POST request should match
        let route_match = router
            .select_backend(HttpMethod::POST, "/api/test", None, None)
            .expect("Should match POST request");
        assert_eq!(
            Ipv4Addr::from(route_match.backend.ipv4),
            Ipv4Addr::new(10, 0, 2, 1)
        );

        // GET request should NOT match
        let route_match = router.select_backend(HttpMethod::GET, "/api/test", None, None);
        assert!(
            route_match.is_none(),
            "Should NOT match GET request when route only allows POST"
        );
    }

    #[test]
    fn test_method_match_multiple_methods() {
        // RED: Test matching multiple HTTP methods
        // HTTPRoute spec: can specify multiple methods in matches
        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 2, 2)),
            8080,
            100,
        )];

        // Add route that matches GET, POST, PUT
        let method_matches = vec![HttpMethod::GET, HttpMethod::POST, HttpMethod::PUT];
        router
            .add_route_with_methods(HttpMethod::ALL, "/api/test", backends, method_matches)
            .expect("Should add route with multiple method constraints");

        // GET, POST, PUT should all match
        for method in &[HttpMethod::GET, HttpMethod::POST, HttpMethod::PUT] {
            let route_match = router
                .select_backend(*method, "/api/test", None, None)
                .unwrap_or_else(|| panic!("Should match {:?} request", method));
            assert_eq!(
                Ipv4Addr::from(route_match.backend.ipv4),
                Ipv4Addr::new(10, 0, 2, 2)
            );
        }

        // DELETE should NOT match
        let route_match = router.select_backend(HttpMethod::DELETE, "/api/test", None, None);
        assert!(
            route_match.is_none(),
            "Should NOT match DELETE request when route only allows GET/POST/PUT"
        );
    }

    #[test]
    fn test_method_match_all_methods() {
        // RED: Test matching ALL methods (Gateway API default)
        // HTTPRoute spec: if matches[].method is omitted, match all methods
        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 2, 3)),
            8080,
            100,
        )];

        // Add route with NO method constraints (should match all methods)
        router
            .add_route(HttpMethod::ALL, "/api/test", backends)
            .expect("Should add route without method constraints");

        // All methods should match
        for method in &[
            HttpMethod::GET,
            HttpMethod::POST,
            HttpMethod::PUT,
            HttpMethod::DELETE,
            HttpMethod::HEAD,
            HttpMethod::OPTIONS,
            HttpMethod::PATCH,
        ] {
            let route_match = router
                .select_backend(*method, "/api/test", None, None)
                .unwrap_or_else(|| panic!("Should match {:?} request", method));
            assert_eq!(
                Ipv4Addr::from(route_match.backend.ipv4),
                Ipv4Addr::new(10, 0, 2, 3),
                "Route without method constraints should match {:?}",
                method
            );
        }
    }

    // ============================================
    // Feature 3: HTTPRoute Query Parameter Matching (Extended)
    // ============================================

    #[test]
    fn test_query_param_match_exact() {
        // RED: Test exact query parameter matching
        // HTTPRoute spec: queryParams[].type = "Exact" (default)
        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 3, 1)),
            8080,
            100,
        )];

        // Add route that ONLY matches requests with ?version=v1
        let query_param_matches = vec![QueryParamMatch {
            name: "version".to_string(),
            value: "v1".to_string(),
            match_type: QueryParamMatchType::Exact,
        }];

        router
            .add_route_with_query_params(
                HttpMethod::GET,
                "/api/data",
                backends,
                query_param_matches,
            )
            .expect("Should add route with query param matching");

        // Request WITH matching query param should succeed
        let route_match = router
            .select_backend_with_query_params(
                HttpMethod::GET,
                "/api/data",
                vec![("version", "v1")],
                None,
                None,
            )
            .expect("Should match with correct query param");

        assert_eq!(
            Ipv4Addr::from(route_match.backend.ipv4),
            Ipv4Addr::new(10, 0, 3, 1)
        );

        // Request WITHOUT matching query param should fail
        let route_match = router.select_backend_with_query_params(
            HttpMethod::GET,
            "/api/data",
            vec![("version", "v2")],
            None,
            None,
        );

        assert!(
            route_match.is_none(),
            "Should NOT match with incorrect query param value"
        );
    }

    #[test]
    fn test_query_param_match_regex() {
        // RED: Test regex query parameter matching
        // HTTPRoute spec: queryParams[].type = "RegularExpression"
        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 3, 2)),
            8080,
            100,
        )];

        // Add route with regex: page matches digits only
        let query_param_matches = vec![QueryParamMatch {
            name: "page".to_string(),
            value: r"^\d+$".to_string(),
            match_type: QueryParamMatchType::RegularExpression,
        }];

        router
            .add_route_with_query_params(
                HttpMethod::GET,
                "/api/list",
                backends,
                query_param_matches,
            )
            .expect("Should add route with regex query params");

        // Requests WITH matching query params (page=1, page=99) should succeed
        for page in &["1", "99", "12345"] {
            let route_match = router
                .select_backend_with_query_params(
                    HttpMethod::GET,
                    "/api/list",
                    vec![("page", page)],
                    None,
                    None,
                )
                .unwrap_or_else(|| panic!("Should match page={}", page));

            assert_eq!(
                Ipv4Addr::from(route_match.backend.ipv4),
                Ipv4Addr::new(10, 0, 3, 2)
            );
        }

        // Request with non-numeric page should fail
        let route_match = router.select_backend_with_query_params(
            HttpMethod::GET,
            "/api/list",
            vec![("page", "abc")],
            None,
            None,
        );

        assert!(
            route_match.is_none(),
            "Should NOT match query param that doesn't match regex"
        );
    }

    #[test]
    fn test_query_param_match_multiple() {
        // RED: Test multiple query parameter matching (AND logic)
        // HTTPRoute spec: ALL queryParams must match
        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 3, 3)),
            8080,
            100,
        )];

        // Add route requiring BOTH version=v1 AND format=json
        let query_param_matches = vec![
            QueryParamMatch {
                name: "version".to_string(),
                value: "v1".to_string(),
                match_type: QueryParamMatchType::Exact,
            },
            QueryParamMatch {
                name: "format".to_string(),
                value: "json".to_string(),
                match_type: QueryParamMatchType::Exact,
            },
        ];

        router
            .add_route_with_query_params(
                HttpMethod::GET,
                "/api/export",
                backends,
                query_param_matches,
            )
            .expect("Should add route with multiple query params");

        // Request with BOTH query params should match
        let route_match = router
            .select_backend_with_query_params(
                HttpMethod::GET,
                "/api/export",
                vec![("version", "v1"), ("format", "json")],
                None,
                None,
            )
            .expect("Should match with all query params present");

        assert_eq!(
            Ipv4Addr::from(route_match.backend.ipv4),
            Ipv4Addr::new(10, 0, 3, 3)
        );

        // Request with only ONE query param should fail
        let route_match = router.select_backend_with_query_params(
            HttpMethod::GET,
            "/api/export",
            vec![("version", "v1")],
            None,
            None,
        );

        assert!(
            route_match.is_none(),
            "Should NOT match when missing required query param"
        );
    }

    // Feature 4: Request Header Filters (Gateway API HTTPRouteBackendRequestHeaderModification)

    #[tokio::test]
    async fn test_filter_set_header() {
        use crate::proxy::filters::RequestHeaderModifier;

        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];

        // Create filter that sets X-Custom-Header
        let filter = RequestHeaderModifier::new()
            .set("X-Custom-Header".to_string(), "test-value".to_string());

        router
            .add_route_with_filters(HttpMethod::GET, "/api/users", backends, filter)
            .expect("Should add route with filters");

        // Select backend and verify filter is attached
        let route_match = router
            .select_backend(HttpMethod::GET, "/api/users", None, None)
            .expect("Should find backend");

        assert!(
            route_match.request_filters.is_some(),
            "RouteMatch should have filters attached"
        );

        let filters = route_match.request_filters.unwrap();
        assert_eq!(
            filters.operations.len(),
            1,
            "Should have 1 filter operation"
        );
    }

    #[tokio::test]
    async fn test_filter_add_header() {
        use crate::proxy::filters::RequestHeaderModifier;

        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];

        // Create filter that adds multiple X-Trace-Id headers
        let filter = RequestHeaderModifier::new()
            .add("X-Trace-Id".to_string(), "trace-1".to_string())
            .add("X-Trace-Id".to_string(), "trace-2".to_string());

        router
            .add_route_with_filters(HttpMethod::POST, "/api/events", backends, filter)
            .expect("Should add route with filters");

        // Select backend and verify filters
        let route_match = router
            .select_backend(HttpMethod::POST, "/api/events", None, None)
            .expect("Should find backend");

        assert!(
            route_match.request_filters.is_some(),
            "RouteMatch should have filters attached"
        );

        let filters = route_match.request_filters.unwrap();
        assert_eq!(
            filters.operations.len(),
            2,
            "Should have 2 filter operations"
        );
    }

    #[tokio::test]
    async fn test_filter_remove_header() {
        use crate::proxy::filters::RequestHeaderModifier;

        let router = Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];

        // Create filter that removes Authorization header
        let filter = RequestHeaderModifier::new().remove("Authorization".to_string());

        router
            .add_route_with_filters(HttpMethod::DELETE, "/api/secure", backends, filter)
            .expect("Should add route with filters");

        // Select backend and verify filter
        let route_match = router
            .select_backend(HttpMethod::DELETE, "/api/secure", None, None)
            .expect("Should find backend");

        assert!(
            route_match.request_filters.is_some(),
            "RouteMatch should have filters attached"
        );

        let filters = route_match.request_filters.unwrap();
        assert_eq!(
            filters.operations.len(),
            1,
            "Should have 1 filter operation"
        );
    }
}
