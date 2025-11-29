//! Dynamic Listener Management for Gateway API
//!
//! **Shared Listener Architecture**
//!
//! Multiple Gateways can share the same listener port. This is critical for
//! Gateway API conformance where many Gateways listen on standard ports (80/443).
//!
//! Design:
//! - One TCP listener per port (not per Gateway)
//! - Multiple Gateways register with the same port
//! - Routing based on hostname + path matching
//! - Reference counting: listener shutdown when last Gateway removed

use crate::proxy::router::Router;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{debug, error, info};

/// Listener protocol
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(clippy::upper_case_acronyms)]
pub enum Protocol {
    HTTP,
    HTTPS,
}

/// Gateway reference for tracking which Gateways use a listener
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GatewayRef {
    pub namespace: String,
    pub name: String,
    pub listener_name: String,
    pub hostname: Option<String>,
}

impl GatewayRef {
    /// Generate unique ID for this Gateway listener
    pub fn id(&self) -> String {
        format!("{}/{}/{}", self.namespace, self.name, self.listener_name)
    }
}

/// Listener configuration
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ListenerConfig {
    pub port: u16,
    pub protocol: Protocol,
}

impl ListenerConfig {
    pub fn bind_addr(&self) -> String {
        format!("0.0.0.0:{}", self.port)
    }
}

/// Active listener with reference counting
struct ActiveListener {
    config: ListenerConfig,
    /// Gateways using this listener
    gateways: HashSet<GatewayRef>,
    cancel_tx: tokio::sync::oneshot::Sender<()>,
    task_handle: tokio::task::JoinHandle<()>,
}

/// Listener Manager - manages shared TCP listeners
pub struct ListenerManager {
    /// Active listeners indexed by port
    listeners: Arc<RwLock<HashMap<u16, ActiveListener>>>,
    /// Shared router for all listeners
    router: Arc<Router>,
    /// Rate limiter for request throttling
    rate_limiter: Arc<crate::proxy::rate_limiter::RateLimiter>,
    /// Circuit breaker for backend health management
    circuit_breaker: Arc<crate::proxy::circuit_breaker::CircuitBreakerManager>,
}

#[allow(dead_code)] // Methods used in Gateway reconciliation and tests
impl ListenerManager {
    /// Create new ListenerManager
    pub fn new(
        router: Arc<Router>,
        rate_limiter: Arc<crate::proxy::rate_limiter::RateLimiter>,
        circuit_breaker: Arc<crate::proxy::circuit_breaker::CircuitBreakerManager>,
    ) -> Self {
        Self {
            listeners: Arc::new(RwLock::new(HashMap::new())),
            router,
            rate_limiter,
            circuit_breaker,
        }
    }

    /// Register a Gateway with a listener port
    ///
    /// If the port listener doesn't exist, creates it.
    /// If it exists, adds this Gateway to the reference set.
    ///
    /// Returns Ok(()) on success, Err(String) if bind fails.
    pub async fn register_gateway(
        &self,
        config: ListenerConfig,
        gateway: GatewayRef,
    ) -> Result<(), String> {
        let mut listeners = self.listeners.write().await;

        if let Some(listener) = listeners.get_mut(&config.port) {
            // Listener exists - validate protocol matches
            if listener.config.protocol != config.protocol {
                return Err(format!(
                    "Port {} already bound with protocol {:?}, cannot bind with {:?}",
                    config.port, listener.config.protocol, config.protocol
                ));
            }

            // Add Gateway to reference set
            if listener.gateways.insert(gateway.clone()) {
                info!(
                    "Gateway {} registered with existing listener on port {}",
                    gateway.id(),
                    config.port
                );
            } else {
                debug!(
                    "Gateway {} already registered on port {}",
                    gateway.id(),
                    config.port
                );
            }

            return Ok(());
        }

        // Create new listener
        info!(
            "Creating new {:?} listener on port {} for Gateway {}",
            config.protocol,
            config.port,
            gateway.id()
        );

        let active_listener = self.spawn_listener(config.clone()).await?;

        // Insert with initial Gateway reference
        let mut gateways = HashSet::new();
        gateways.insert(gateway);

        listeners.insert(
            config.port,
            ActiveListener {
                config,
                gateways,
                cancel_tx: active_listener.cancel_tx,
                task_handle: active_listener.task_handle,
            },
        );

        Ok(())
    }

    /// Unregister a Gateway from a listener port
    ///
    /// If this is the last Gateway using the port, shuts down the listener.
    ///
    /// Returns Ok(()) on success, Err(String) if Gateway not found.
    pub async fn unregister_gateway(&self, port: u16, gateway: &GatewayRef) -> Result<(), String> {
        let mut listeners = self.listeners.write().await;

        if let Some(listener) = listeners.get_mut(&port) {
            if !listener.gateways.remove(gateway) {
                return Err(format!(
                    "Gateway {} not registered on port {}",
                    gateway.id(),
                    port
                ));
            }

            info!("Gateway {} unregistered from port {}", gateway.id(), port);

            // If last Gateway removed, shutdown listener
            if listener.gateways.is_empty() {
                info!(
                    "Last Gateway removed from port {}, shutting down listener",
                    port
                );
                if let Some(listener) = listeners.remove(&port) {
                    let _ = listener.cancel_tx.send(());
                }
            }

            return Ok(());
        }

        Err(format!("No listener on port {}", port))
    }

    /// List all active listeners with their Gateway counts
    pub async fn list_listeners(&self) -> Vec<(u16, Protocol, usize)> {
        let listeners = self.listeners.read().await;
        listeners
            .iter()
            .map(|(port, active)| (*port, active.config.protocol.clone(), active.gateways.len()))
            .collect()
    }

    /// Shutdown all listeners gracefully
    pub async fn shutdown(&self) -> Result<(), String> {
        info!("Shutting down all listeners");
        let mut listeners = self.listeners.write().await;

        for (port, listener) in listeners.drain() {
            info!("Shutting down listener on port {}", port);
            let _ = listener.cancel_tx.send(());
        }

        Ok(())
    }

    /// Spawn a listener task
    async fn spawn_listener(&self, config: ListenerConfig) -> Result<ActiveListener, String> {
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();

        let router = self.router.clone();
        let rate_limiter = self.rate_limiter.clone();
        let circuit_breaker = self.circuit_breaker.clone();
        let bind_addr = config.bind_addr();
        let protocol = config.protocol.clone();

        // Try to bind immediately to detect port conflicts
        let listener = TcpListener::bind(&bind_addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", bind_addr, e))?;

        let task_handle = tokio::spawn(async move {
            info!("Listener bound to {} ({:?})", bind_addr, protocol);

            loop {
                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((stream, peer_addr)) => {
                                debug!("Accepted connection from {} on {}", peer_addr, bind_addr);

                                let router = router.clone();
                                let rate_limiter = rate_limiter.clone();
                                let circuit_breaker = circuit_breaker.clone();
                                let protocol = protocol.clone();

                                // Spawn connection handler
                                tokio::spawn(async move {
                                    let io = TokioIo::new(stream);

                                    // Create service that uses the router
                                    let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                                        let router = router.clone();
                                        let rate_limiter = rate_limiter.clone();
                                        let circuit_breaker = circuit_breaker.clone();
                                        async move {
                                            Self::handle_request(req, router, rate_limiter, circuit_breaker).await
                                        }
                                    });

                                    // Serve connection based on protocol
                                    let result = match protocol {
                                        Protocol::HTTP => {
                                            http1::Builder::new()
                                                .serve_connection(io, service)
                                                .await
                                        }
                                        Protocol::HTTPS => {
                                            // TODO: TLS termination
                                            http1::Builder::new()
                                                .serve_connection(io, service)
                                                .await
                                        }
                                    };

                                    if let Err(e) = result {
                                        debug!("Connection error: {}", e);
                                    }
                                });
                            }
                            Err(e) => {
                                error!("Accept error on {}: {}", bind_addr, e);
                            }
                        }
                    }
                    _ = &mut cancel_rx => {
                        info!("Listener on {} received shutdown signal", bind_addr);
                        break;
                    }
                }
            }

            info!("Listener on {} stopped", bind_addr);
        });

        Ok(ActiveListener {
            config,
            gateways: HashSet::new(),
            cancel_tx,
            task_handle,
        })
    }

    /// Handle HTTP request using router with rate limiting and circuit breaking
    async fn handle_request(
        req: Request<hyper::body::Incoming>,
        router: Arc<Router>,
        rate_limiter: Arc<crate::proxy::rate_limiter::RateLimiter>,
        circuit_breaker: Arc<crate::proxy::circuit_breaker::CircuitBreakerManager>,
    ) -> Result<
        Response<http_body_util::combinators::BoxBody<hyper::body::Bytes, std::io::Error>>,
        hyper::Error,
    > {
        use http_body_util::{BodyExt, Full};
        use hyper::body::Bytes;

        let method = req.method().clone();
        let uri = req.uri().clone();
        let path = uri.path();
        let headers = req.headers();

        debug!("Request: {} {}", method, path);

        // Extract Host header for routing
        let host_header = headers
            .get("host")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");

        // Convert hyper Method to router HttpMethod
        let http_method = match *req.method() {
            hyper::Method::GET => common::HttpMethod::GET,
            hyper::Method::POST => common::HttpMethod::POST,
            hyper::Method::PUT => common::HttpMethod::PUT,
            hyper::Method::DELETE => common::HttpMethod::DELETE,
            hyper::Method::PATCH => common::HttpMethod::PATCH,
            hyper::Method::HEAD => common::HttpMethod::HEAD,
            hyper::Method::OPTIONS => common::HttpMethod::OPTIONS,
            _ => {
                // Response builder with static values should never fail
                #[allow(clippy::expect_used)]
                return Ok(Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .body(
                        Full::new(Bytes::from("Method Not Allowed"))
                            .map_err(std::io::Error::other)
                            .boxed(),
                    )
                    .expect("Building 405 response should never fail"));
            }
        };

        // Select backend using router
        let route_match = router.select_backend_with_headers(
            http_method,
            path,
            vec![("host", host_header)],
            None, // src_ip - TODO: extract from connection
            None, // src_port
        );

        let route_match = match route_match {
            Some(m) => m,
            None => {
                debug!(
                    "No route found for {} {} (host: {})",
                    method, path, host_header
                );
                // Response builder with static values should never fail
                #[allow(clippy::expect_used)]
                return Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(
                        Full::new(Bytes::from("Not Found"))
                            .map_err(std::io::Error::other)
                            .boxed(),
                    )
                    .expect("Building 404 response should never fail"));
            }
        };

        let backend = route_match.backend;
        let pattern = route_match.pattern;

        // Check rate limit for this route
        if !rate_limiter.check_rate_limit(&pattern) {
            debug!("Rate limit exceeded for {} {}", method, path);
            // Response builder with static values should never fail
            #[allow(clippy::expect_used)]
            return Ok(Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .body(
                    Full::new(Bytes::from("Too Many Requests"))
                        .map_err(std::io::Error::other)
                        .boxed(),
                )
                .expect("Building 429 response should never fail"));
        }

        // Check circuit breaker for backend
        // Generate backend ID supporting both IPv4 and IPv6
        let backend_id = if let Some(ipv4) = backend.as_ipv4() {
            format!("{}:{}", ipv4, backend.port)
        } else if let Some(ipv6) = backend.as_ipv6() {
            format!("[{}]:{}", ipv6, backend.port)
        } else {
            format!("unknown:{}", backend.port)
        };

        if !circuit_breaker.allow_request(&backend_id) {
            debug!("Circuit breaker open for backend {}", backend_id);
            // Response builder with static values should never fail
            #[allow(clippy::expect_used)]
            return Ok(Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(
                    Full::new(Bytes::from("Service Unavailable"))
                        .map_err(std::io::Error::other)
                        .boxed(),
                )
                .expect("Building 503 response should never fail"));
        }

        // Convert backend IP to SocketAddr
        let backend_addr = if let Some(ipv4) = backend.as_ipv4() {
            std::net::SocketAddr::from((ipv4, backend.port))
        } else if let Some(ipv6) = backend.as_ipv6() {
            std::net::SocketAddr::from((ipv6, backend.port))
        } else {
            error!("Backend has invalid IP address");
            // Response builder with static values should never fail
            #[allow(clippy::expect_used)]
            return Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(
                    Full::new(Bytes::from("Internal Server Error"))
                        .map_err(std::io::Error::other)
                        .boxed(),
                )
                .expect("Building 500 response should never fail"));
        };

        debug!("Proxying {} {} to {}", method, path, backend_addr);

        // Connect to backend
        let stream = match tokio::net::TcpStream::connect(backend_addr).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to connect to backend {}: {}", backend_addr, e);
                // Response builder with static values should never fail
                #[allow(clippy::expect_used)]
                return Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(
                        Full::new(Bytes::from("Bad Gateway"))
                            .map_err(std::io::Error::other)
                            .boxed(),
                    )
                    .expect("Building 502 response should never fail"));
            }
        };

        let io = TokioIo::new(stream);

        // Create HTTP/1 connection to backend
        let (mut sender, conn) = match hyper::client::conn::http1::handshake(io).await {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to handshake with backend {}: {}", backend_addr, e);
                // Response builder with static values should never fail
                #[allow(clippy::expect_used)]
                return Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(
                        Full::new(Bytes::from("Bad Gateway"))
                            .map_err(std::io::Error::other)
                            .boxed(),
                    )
                    .expect("Building 502 response should never fail"));
            }
        };

        // Spawn connection driver
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                debug!("Backend connection error: {}", e);
            }
        });

        // Forward request to backend
        let backend_response = match sender.send_request(req).await {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to send request to backend {}: {}", backend_addr, e);
                // Record failure for circuit breaker
                circuit_breaker.record_failure(&backend_id);
                // Response builder with static values should never fail
                #[allow(clippy::expect_used)]
                return Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(
                        Full::new(Bytes::from("Bad Gateway"))
                            .map_err(std::io::Error::other)
                            .boxed(),
                    )
                    .expect("Building 502 response should never fail"));
            }
        };

        // Record success or failure based on status code
        let status = backend_response.status();
        if status.is_server_error() {
            // 5xx errors indicate backend failure
            circuit_breaker.record_failure(&backend_id);
        } else {
            // 2xx, 3xx, 4xx are considered successful (backend is responsive)
            circuit_breaker.record_success(&backend_id);
        }

        // Return backend response
        Ok(backend_response.map(|body| body.map_err(std::io::Error::other).boxed()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Test helper: create a ListenerManager with default rate limiter and circuit breaker
    fn create_test_manager(router: Arc<Router>) -> ListenerManager {
        let rate_limiter = Arc::new(crate::proxy::rate_limiter::RateLimiter::new());
        let circuit_breaker = Arc::new(crate::proxy::circuit_breaker::CircuitBreakerManager::new(
            5,
            std::time::Duration::from_secs(30),
        ));
        ListenerManager::new(router, rate_limiter, circuit_breaker)
    }

    #[tokio::test]
    async fn test_shared_listener_multiple_gateways_same_port() {
        let router = Arc::new(Router::new());
        let manager = create_test_manager(router);

        let config = ListenerConfig {
            port: 0, // OS assigns port
            protocol: Protocol::HTTP,
        };

        let gateway1 = GatewayRef {
            namespace: "default".to_string(),
            name: "gateway-1".to_string(),
            listener_name: "http".to_string(),
            hostname: None,
        };

        let gateway2 = GatewayRef {
            namespace: "conformance".to_string(),
            name: "gateway-2".to_string(),
            listener_name: "http".to_string(),
            hostname: Some("*.example.com".to_string()),
        };

        // Register first Gateway
        manager
            .register_gateway(config.clone(), gateway1.clone())
            .await
            .expect("First Gateway should succeed");

        // Get the actual assigned port
        let listeners = manager.list_listeners().await;
        assert_eq!(listeners.len(), 1, "Should have 1 listener");
        let actual_port = listeners[0].0;
        assert_eq!(listeners[0].2, 1, "Should have 1 Gateway registered");

        // Register second Gateway on SAME port - should SUCCEED
        let config_with_port = ListenerConfig {
            port: actual_port,
            protocol: Protocol::HTTP,
        };

        manager
            .register_gateway(config_with_port, gateway2.clone())
            .await
            .expect("Second Gateway should share port");

        // Verify still only 1 listener, but 2 Gateways
        let listeners = manager.list_listeners().await;
        assert_eq!(listeners.len(), 1, "Should still have 1 listener");
        assert_eq!(listeners[0].2, 2, "Should have 2 Gateways registered");
    }

    #[tokio::test]
    async fn test_unregister_gateway_keeps_listener_alive() {
        let router = Arc::new(Router::new());
        let manager = create_test_manager(router);

        let config = ListenerConfig {
            port: 0,
            protocol: Protocol::HTTP,
        };

        let gateway1 = GatewayRef {
            namespace: "default".to_string(),
            name: "gateway-1".to_string(),
            listener_name: "http".to_string(),
            hostname: None,
        };

        let gateway2 = GatewayRef {
            namespace: "default".to_string(),
            name: "gateway-2".to_string(),
            listener_name: "http".to_string(),
            hostname: None,
        };

        manager
            .register_gateway(config.clone(), gateway1.clone())
            .await
            .unwrap();

        let actual_port = manager.list_listeners().await[0].0;
        let config_with_port = ListenerConfig {
            port: actual_port,
            protocol: Protocol::HTTP,
        };

        manager
            .register_gateway(config_with_port, gateway2.clone())
            .await
            .unwrap();

        // Unregister first Gateway
        manager
            .unregister_gateway(actual_port, &gateway1)
            .await
            .expect("Should unregister gateway1");

        // Listener should still exist (gateway2 still using it)
        let listeners = manager.list_listeners().await;
        assert_eq!(listeners.len(), 1, "Listener should still exist");
        assert_eq!(listeners[0].2, 1, "Should have 1 Gateway left");
    }

    #[tokio::test]
    async fn test_unregister_last_gateway_shuts_down_listener() {
        let router = Arc::new(Router::new());
        let manager = create_test_manager(router);

        let config = ListenerConfig {
            port: 0,
            protocol: Protocol::HTTP,
        };

        let gateway = GatewayRef {
            namespace: "default".to_string(),
            name: "gateway-1".to_string(),
            listener_name: "http".to_string(),
            hostname: None,
        };

        manager
            .register_gateway(config, gateway.clone())
            .await
            .unwrap();

        let actual_port = manager.list_listeners().await[0].0;

        // Unregister only Gateway
        manager
            .unregister_gateway(actual_port, &gateway)
            .await
            .expect("Should unregister gateway");

        // Listener should be shut down
        let listeners = manager.list_listeners().await;
        assert_eq!(listeners.len(), 0, "Listener should be shut down");
    }

    #[tokio::test]
    async fn test_protocol_mismatch_rejected() {
        let router = Arc::new(Router::new());
        let manager = create_test_manager(router);

        let http_config = ListenerConfig {
            port: 0,
            protocol: Protocol::HTTP,
        };

        let gateway1 = GatewayRef {
            namespace: "default".to_string(),
            name: "gateway-1".to_string(),
            listener_name: "http".to_string(),
            hostname: None,
        };

        manager
            .register_gateway(http_config, gateway1)
            .await
            .unwrap();

        let actual_port = manager.list_listeners().await[0].0;

        // Try to register HTTPS on same port - should FAIL
        let https_config = ListenerConfig {
            port: actual_port,
            protocol: Protocol::HTTPS,
        };

        let gateway2 = GatewayRef {
            namespace: "default".to_string(),
            name: "gateway-2".to_string(),
            listener_name: "https".to_string(),
            hostname: None,
        };

        let result = manager.register_gateway(https_config, gateway2).await;
        assert!(
            result.is_err(),
            "Should reject protocol mismatch on same port"
        );
        assert!(result.unwrap_err().contains("already bound with protocol"));
    }

    #[tokio::test]
    async fn test_idempotent_registration() {
        let router = Arc::new(Router::new());
        let manager = create_test_manager(router);

        let config = ListenerConfig {
            port: 0,
            protocol: Protocol::HTTP,
        };

        let gateway = GatewayRef {
            namespace: "default".to_string(),
            name: "gateway-1".to_string(),
            listener_name: "http".to_string(),
            hostname: None,
        };

        manager
            .register_gateway(config.clone(), gateway.clone())
            .await
            .unwrap();

        let actual_port = manager.list_listeners().await[0].0;
        let config_with_port = ListenerConfig {
            port: actual_port,
            protocol: Protocol::HTTP,
        };

        // Register same Gateway again - should be idempotent
        manager
            .register_gateway(config_with_port, gateway)
            .await
            .unwrap();

        // Should still have 1 listener with 1 Gateway
        let listeners = manager.list_listeners().await;
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].2, 1, "Should still have only 1 Gateway");
    }
}
