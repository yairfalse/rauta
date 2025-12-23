//! HTTP Server - proxies requests to backends
//!
//! This module contains the ProxyServer struct and connection handling logic.
//! Request handling, forwarding, and metrics are delegated to separate modules:
//! - `metrics.rs` - Prometheus metrics
//! - `forwarder.rs` - Backend request forwarding
//! - `request_handler.rs` - Request routing and processing

use crate::proxy::backend_pool::BackendConnectionPools;
use crate::proxy::circuit_breaker::CircuitBreakerManager;
use crate::proxy::forwarder::{BackendPools, ProtocolCache, Workers};
use crate::proxy::rate_limiter::RateLimiter;
use crate::proxy::request_handler::{handle_request, WorkerSelector};
use crate::proxy::router::Router;
use crate::proxy::worker::Worker;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::{http1, http2};
use hyper::service::service_fn;
use hyper::Request;
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// HTTP Proxy Server
pub struct ProxyServer {
    bind_addr: String,
    router: Arc<Router>,
    client: Client<HttpConnector, Full<Bytes>>,
    backend_pools: BackendPools,
    protocol_cache: ProtocolCache,
    #[allow(dead_code)]
    workers: Option<Workers>,
    #[allow(dead_code)]
    worker_selector: Option<Arc<WorkerSelector>>,
    rate_limiter: Arc<RateLimiter>,
    circuit_breaker: Arc<CircuitBreakerManager>,
}

impl ProxyServer {
    /// Create new proxy server (legacy method - use new_with_workers for better performance)
    #[allow(dead_code)]
    pub fn new(bind_addr: String, router: Arc<Router>) -> Result<Self, String> {
        let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
        let backend_pools = Arc::new(Mutex::new(BackendConnectionPools::new(0)));
        let protocol_cache = Arc::new(Mutex::new(HashMap::new()));
        let rate_limiter = Arc::new(RateLimiter::new());
        let circuit_breaker = Arc::new(CircuitBreakerManager::new(
            5,
            std::time::Duration::from_secs(30),
        ));

        Ok(Self {
            bind_addr,
            router,
            client,
            backend_pools,
            protocol_cache,
            workers: None,
            worker_selector: None,
            rate_limiter,
            circuit_breaker,
        })
    }

    /// Create new proxy server with per-core workers (lock-free!)
    #[allow(dead_code)]
    pub fn new_with_workers(
        bind_addr: String,
        router: Arc<Router>,
        num_workers: usize,
    ) -> Result<Self, String> {
        let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();

        let mut workers = Vec::with_capacity(num_workers);
        for id in 0..num_workers {
            workers.push(Worker::new(id, router.clone()));
        }

        let protocol_cache = Arc::new(Mutex::new(HashMap::new()));
        let rate_limiter = Arc::new(RateLimiter::new());
        let circuit_breaker = Arc::new(CircuitBreakerManager::new(
            5,
            std::time::Duration::from_secs(30),
        ));

        Ok(Self {
            bind_addr,
            router,
            client,
            backend_pools: Arc::new(Mutex::new(BackendConnectionPools::new(0))),
            protocol_cache,
            workers: Some(Arc::new(workers)),
            worker_selector: Some(Arc::new(WorkerSelector::new(num_workers))),
            rate_limiter,
            circuit_breaker,
        })
    }

    /// Mark a backend as supporting HTTP/2
    #[allow(dead_code)]
    pub async fn set_backend_protocol_http2(&self, backend_host: &str, backend_port: u16) {
        let backend_key = format!("{}:{}", backend_host, backend_port);
        let mut cache = self.protocol_cache.lock().await;
        cache.insert(backend_key, true);
    }

    /// Start serving HTTP requests
    #[allow(dead_code)]
    pub async fn serve(self) -> Result<(), String> {
        let listener = TcpListener::bind(&self.bind_addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", self.bind_addr, e))?;

        loop {
            let (stream, _) = listener
                .accept()
                .await
                .map_err(|e| format!("Failed to accept connection: {}", e))?;

            let router = self.router.clone();
            let client = self.client.clone();
            let backend_pools = self.backend_pools.clone();
            let protocol_cache = self.protocol_cache.clone();
            let workers = self.workers.clone();
            let worker_selector = self.worker_selector.clone();
            let rate_limiter = self.rate_limiter.clone();
            let circuit_breaker = self.circuit_breaker.clone();

            tokio::spawn(async move {
                let io = TokioIo::new(stream);

                let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let router = router.clone();
                    let client = client.clone();
                    let backend_pools = backend_pools.clone();
                    let protocol_cache = protocol_cache.clone();
                    let workers = workers.clone();
                    let worker_selector = worker_selector.clone();
                    let rate_limiter = rate_limiter.clone();
                    let circuit_breaker = circuit_breaker.clone();
                    async move {
                        handle_request(
                            req,
                            router,
                            client,
                            backend_pools,
                            protocol_cache,
                            workers,
                            worker_selector,
                            rate_limiter,
                            circuit_breaker,
                        )
                        .await
                    }
                });

                if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                    debug!("Error serving connection: {}", e);
                }
            });
        }
    }

    /// Start serving HTTP requests with graceful shutdown support
    #[allow(dead_code)]
    pub async fn serve_with_shutdown<F>(self, shutdown_signal: F) -> Result<(), String>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        use tokio::sync::Notify;

        let listener = TcpListener::bind(&self.bind_addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", self.bind_addr, e))?;

        let active_connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let shutdown_notify = Arc::new(Notify::new());

        let active_connections_clone = active_connections.clone();
        let shutdown_notify_clone = shutdown_notify.clone();
        tokio::spawn(async move {
            shutdown_signal.await;
            info!("Graceful shutdown initiated - draining active connections");

            let drain_timeout = tokio::time::Duration::from_secs(30);
            let start = std::time::Instant::now();

            while active_connections_clone.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                if start.elapsed() > drain_timeout {
                    warn!(
                        "Drain timeout exceeded - forcing shutdown with {} active connections",
                        active_connections_clone.load(std::sync::atomic::Ordering::Relaxed)
                    );
                    break;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }

            info!("All connections drained - shutting down");
            shutdown_notify_clone.notify_one();
        });

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _)) => {
                            active_connections.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                            let router = self.router.clone();
                            let client = self.client.clone();
                            let backend_pools = self.backend_pools.clone();
                            let protocol_cache = self.protocol_cache.clone();
                            let workers = self.workers.clone();
                            let worker_selector = self.worker_selector.clone();
                            let rate_limiter = self.rate_limiter.clone();
                            let circuit_breaker = self.circuit_breaker.clone();
                            let active_connections_clone = active_connections.clone();

                            tokio::spawn(async move {
                                let io = TokioIo::new(stream);

                                let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                                    let router = router.clone();
                                    let client = client.clone();
                                    let backend_pools = backend_pools.clone();
                                    let protocol_cache = protocol_cache.clone();
                                    let workers = workers.clone();
                                    let worker_selector = worker_selector.clone();
                                    let rate_limiter = rate_limiter.clone();
                                    let circuit_breaker = circuit_breaker.clone();
                                    async move {
                                        handle_request(
                                            req,
                                            router,
                                            client,
                                            backend_pools,
                                            protocol_cache,
                                            workers,
                                            worker_selector,
                                            rate_limiter,
                                            circuit_breaker,
                                        )
                                        .await
                                    }
                                });

                                if let Err(e) = http1::Builder::new()
                                    .serve_connection(io, service)
                                    .await
                                {
                                    debug!("Error serving connection: {}", e);
                                }

                                active_connections_clone.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                            });
                        }
                        Err(e) => {
                            return Err(format!("Failed to accept connection: {}", e));
                        }
                    }
                }
                _ = shutdown_notify.notified() => {
                    info!("Server shutdown complete");
                    return Ok(());
                }
            }
        }
    }

    /// Start serving HTTPS requests with TLS termination
    #[allow(dead_code)]
    pub async fn serve_https(
        self,
        sni_resolver: crate::proxy::tls::SniResolver,
    ) -> Result<(), String> {
        use tokio_rustls::TlsAcceptor;

        let listener = TcpListener::bind(&self.bind_addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", self.bind_addr, e))?;

        info!("HTTPS proxy server listening on {}", self.bind_addr);

        let server_config = sni_resolver
            .to_server_config()
            .map_err(|e| format!("Failed to build TLS config: {}", e))?;

        let acceptor = TlsAcceptor::from(server_config);

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                    continue;
                }
            };

            let router = self.router.clone();
            let client = self.client.clone();
            let backend_pools = self.backend_pools.clone();
            let protocol_cache = self.protocol_cache.clone();
            let workers = self.workers.clone();
            let worker_selector = self.worker_selector.clone();
            let rate_limiter = self.rate_limiter.clone();
            let circuit_breaker = self.circuit_breaker.clone();
            let acceptor_clone = acceptor.clone();

            tokio::spawn(async move {
                let tls_stream = match acceptor_clone.accept(stream).await {
                    Ok(s) => s,
                    Err(e) => {
                        error!("TLS handshake failed: {}", e);
                        return;
                    }
                };

                let io = TokioIo::new(tls_stream);

                let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let router = router.clone();
                    let client = client.clone();
                    let backend_pools = backend_pools.clone();
                    let protocol_cache = protocol_cache.clone();
                    let workers = workers.clone();
                    let worker_selector = worker_selector.clone();
                    let rate_limiter = rate_limiter.clone();
                    let circuit_breaker = circuit_breaker.clone();
                    async move {
                        handle_request(
                            req,
                            router,
                            client,
                            backend_pools,
                            protocol_cache,
                            workers,
                            worker_selector,
                            rate_limiter,
                            circuit_breaker,
                        )
                        .await
                    }
                });

                let _ = http1::Builder::new().serve_connection(io, service).await;
            });
        }
    }

    /// Start serving HTTPS requests with HTTP/2 support (ALPN negotiation)
    #[allow(dead_code)]
    pub async fn serve_https_h2(
        self,
        sni_resolver: crate::proxy::tls::SniResolver,
    ) -> Result<(), String> {
        use tokio_rustls::TlsAcceptor;

        let listener = TcpListener::bind(&self.bind_addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", self.bind_addr, e))?;

        info!(
            "HTTPS/H2 proxy server listening on {} (ALPN: h2, http/1.1)",
            self.bind_addr
        );

        let mut server_config = sni_resolver
            .to_server_config()
            .map_err(|e| format!("Failed to build TLS config: {}", e))?;

        let server_config_mut = Arc::make_mut(&mut server_config);
        server_config_mut.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

        let acceptor = TlsAcceptor::from(server_config);

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                    continue;
                }
            };

            let router = self.router.clone();
            let client = self.client.clone();
            let backend_pools = self.backend_pools.clone();
            let protocol_cache = self.protocol_cache.clone();
            let workers = self.workers.clone();
            let worker_selector = self.worker_selector.clone();
            let rate_limiter = self.rate_limiter.clone();
            let circuit_breaker = self.circuit_breaker.clone();
            let acceptor_clone = acceptor.clone();

            tokio::spawn(async move {
                let tls_stream = match acceptor_clone.accept(stream).await {
                    Ok(s) => s,
                    Err(e) => {
                        error!("TLS handshake failed: {}", e);
                        return;
                    }
                };

                let is_h2 = {
                    let (_, conn) = tls_stream.get_ref();
                    conn.alpn_protocol() == Some(b"h2")
                };

                let io = TokioIo::new(tls_stream);

                let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let router = router.clone();
                    let client = client.clone();
                    let backend_pools = backend_pools.clone();
                    let protocol_cache = protocol_cache.clone();
                    let workers = workers.clone();
                    let worker_selector = worker_selector.clone();
                    let rate_limiter = rate_limiter.clone();
                    let circuit_breaker = circuit_breaker.clone();
                    async move {
                        handle_request(
                            req,
                            router,
                            client,
                            backend_pools,
                            protocol_cache,
                            workers,
                            worker_selector,
                            rate_limiter,
                            circuit_breaker,
                        )
                        .await
                    }
                });

                if is_h2 {
                    let _ = http2::Builder::new(TokioExecutor::new())
                        .serve_connection(io, service)
                        .await;
                } else {
                    let _ = http1::Builder::new().serve_connection(io, service).await;
                }
            });
        }
    }

    /// Start serving HTTP/2 requests
    #[allow(dead_code)]
    pub async fn serve_http2(self) -> Result<(), String> {
        let listener = TcpListener::bind(&self.bind_addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", self.bind_addr, e))?;

        info!("HTTP/2 proxy server listening on {}", self.bind_addr);

        loop {
            let (stream, _) = listener
                .accept()
                .await
                .map_err(|e| format!("Failed to accept connection: {}", e))?;

            let router = self.router.clone();
            let client = self.client.clone();
            let backend_pools = self.backend_pools.clone();
            let protocol_cache = self.protocol_cache.clone();
            let workers = self.workers.clone();
            let worker_selector = self.worker_selector.clone();
            let rate_limiter = self.rate_limiter.clone();
            let circuit_breaker = self.circuit_breaker.clone();

            tokio::spawn(async move {
                let io = TokioIo::new(stream);

                let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let router = router.clone();
                    let client = client.clone();
                    let backend_pools = backend_pools.clone();
                    let protocol_cache = protocol_cache.clone();
                    let workers = workers.clone();
                    let worker_selector = worker_selector.clone();
                    let rate_limiter = rate_limiter.clone();
                    let circuit_breaker = circuit_breaker.clone();
                    async move {
                        handle_request(
                            req,
                            router,
                            client,
                            backend_pools,
                            protocol_cache,
                            workers,
                            worker_selector,
                            rate_limiter,
                            circuit_breaker,
                        )
                        .await
                    }
                });

                let _ = http2::Builder::new(TokioExecutor::new())
                    .serve_connection(io, service)
                    .await;
            });
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::proxy::forwarder::is_hop_by_hop_header;
    use crate::proxy::request_handler::{method_to_str, status_to_str};
    use common::{Backend, HttpMethod};
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn test_server_creation() {
        let router = crate::proxy::router::Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(127, 0, 0, 1)),
            9999,
            100,
        )];
        router
            .add_route(HttpMethod::GET, "/test", backends)
            .unwrap();

        let server = ProxyServer::new("127.0.0.1:8080".to_string(), Arc::new(router));
        assert!(server.is_ok());
    }

    // Note: Integration tests for handle_request that need hyper::body::Incoming
    // are in the original test suite and run via the integration test framework.
    // Unit tests here focus on component testing.

    #[test]
    fn test_is_hop_by_hop() {
        assert!(is_hop_by_hop_header("connection"));
        assert!(is_hop_by_hop_header("Connection"));
        assert!(is_hop_by_hop_header("keep-alive"));
        assert!(!is_hop_by_hop_header("content-type"));
    }

    #[test]
    fn test_method_to_str() {
        assert_eq!(method_to_str(&HttpMethod::GET), "GET");
        assert_eq!(method_to_str(&HttpMethod::POST), "POST");
        assert_eq!(method_to_str(&HttpMethod::ALL), "ALL");
    }

    #[test]
    fn test_status_to_str() {
        assert_eq!(status_to_str(200), "200");
        assert_eq!(status_to_str(404), "404");
        assert_eq!(status_to_str(500), "500");
        assert_eq!(status_to_str(999), "other");
    }

    #[tokio::test]
    async fn test_server_with_workers() {
        let router = crate::proxy::router::Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(127, 0, 0, 1)),
            9999,
            100,
        )];
        router
            .add_route(HttpMethod::GET, "/test", backends)
            .unwrap();

        let server =
            ProxyServer::new_with_workers("127.0.0.1:8081".to_string(), Arc::new(router), 4);
        assert!(server.is_ok());

        let server = server.unwrap();
        assert!(server.workers.is_some());
        assert!(server.worker_selector.is_some());
    }
}
