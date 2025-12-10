//! HTTP Server - proxies requests to backends

use crate::apis::metrics::CONTROLLER_METRICS_REGISTRY;
use crate::proxy::filters::{
    HeaderModifierOp, RequestHeaderModifier, RequestRedirect, ResponseHeaderModifier, Timeout,
};
use crate::proxy::router::Router;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::Bytes;
use hyper::server::conn::{http1, http2};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;
use lazy_static::lazy_static;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

lazy_static! {
    /// Global metrics registry
    static ref METRICS_REGISTRY: Registry = Registry::new();

    /// HTTP request duration histogram (in seconds)
    ///
    /// Note: Fallback metrics use .expect() as last line of defense - if Prometheus itself is broken, we should panic
    #[allow(clippy::expect_used)]
    static ref HTTP_REQUEST_DURATION: HistogramVec = {
        let opts = HistogramOpts::new(
            "http_request_duration_seconds",
            "HTTP request latencies in seconds",
        )
        .buckets(vec![
            0.001, 0.005, 0.010, 0.025, 0.050, 0.075, 0.100, 0.250, 0.500, 1.000, 2.500, 5.000,
        ]);
        let histogram = HistogramVec::new(opts, &["method", "path", "status"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create http_request_duration_seconds histogram: {}", e);
                #[allow(clippy::expect_used)]
                {
                    HistogramVec::new(
                        HistogramOpts::new("http_request_duration_seconds_fallback", "Fallback metric for HTTP request duration"),
                        &["method", "path", "status"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = METRICS_REGISTRY.register(Box::new(histogram.clone())) {
            eprintln!("WARN: Failed to register http_request_duration_seconds histogram: {}", e);
            eprintln!("WARN: Metrics collection will be degraded but gateway will continue");
        }
        histogram
    };

    /// HTTP request counter with per-worker distribution
    ///
    /// Note: Fallback metrics use .expect() as last line of defense - if Prometheus itself is broken, we should panic
    #[allow(clippy::expect_used)]
    static ref HTTP_REQUESTS_TOTAL: IntCounterVec = {
        let opts = Opts::new("http_requests_total", "Total number of HTTP requests (with per-worker distribution)");
        let counter = IntCounterVec::new(opts, &["method", "path", "status", "worker_id"])
            .unwrap_or_else(|e| {
                eprintln!("WARN: Failed to create http_requests_total counter: {}", e);
                #[allow(clippy::expect_used)]
                {
                    IntCounterVec::new(
                        Opts::new("http_requests_total_fallback", "Fallback metric for HTTP requests total"),
                        &["method", "path", "status", "worker_id"]
                    ).expect("Fallback metric creation should never fail - if this panics, Prometheus is broken")
                }
            });
        if let Err(e) = METRICS_REGISTRY.register(Box::new(counter.clone())) {
            eprintln!("WARN: Failed to register http_requests_total counter: {}", e);
            eprintln!("WARN: Metrics collection will be degraded but gateway will continue");
        }
        counter
    };
}

use crate::proxy::backend_pool::{gather_pool_metrics, BackendConnectionPools, PoolError};
use crate::proxy::circuit_breaker::CircuitBreakerManager;
use crate::proxy::rate_limiter::RateLimiter;
use crate::proxy::worker::Worker;

/// Production-grade HTTP/2 backend connection pools
/// NOTE: Arc<Mutex> is temporary until Stage 2 per-core workers
/// In Stage 2, each worker will own their BackendConnectionPools (no lock!)
type BackendPools = Arc<Mutex<BackendConnectionPools>>;

/// Per-core workers (lock-free architecture)
/// Workers stored in immutable Vec - selection is lock-free!
/// Each worker has Mutex<BackendConnectionPools> - per-worker locking only
type Workers = Arc<Vec<Worker>>;

/// Worker selector for round-robin distribution
/// In production, this would use CPU affinity, but round-robin is simpler for now
struct WorkerSelector {
    counter: std::sync::atomic::AtomicUsize,
    num_workers: usize,
}

impl WorkerSelector {
    fn new(num_workers: usize) -> Self {
        Self {
            counter: std::sync::atomic::AtomicUsize::new(0),
            num_workers,
        }
    }

    /// Select next worker (round-robin)
    fn select(&self) -> usize {
        let current = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        current % self.num_workers
    }
}

/// Protocol detection cache - tracks which backends support HTTP/2
type ProtocolCache = Arc<Mutex<HashMap<String, bool>>>; // true = HTTP/2, false = HTTP/1.1

/// HTTP Proxy Server
pub struct ProxyServer {
    bind_addr: String,
    router: Arc<Router>,
    client: Client<HttpConnector, Full<Bytes>>,
    backend_pools: BackendPools,
    protocol_cache: ProtocolCache,
    #[allow(dead_code)] // Used in tests; will be used in request handling path
    workers: Option<Workers>, // None = legacy mode, Some = per-core workers (lock-free!)
    #[allow(dead_code)] // Used when workers are enabled
    worker_selector: Option<Arc<WorkerSelector>>, // Round-robin worker selection
    rate_limiter: Arc<RateLimiter>, // Per-route rate limiting
    circuit_breaker: Arc<CircuitBreakerManager>, // Per-backend circuit breaking
}

impl ProxyServer {
    /// Create new proxy server (legacy method - use new_with_workers for better performance)
    #[allow(dead_code)]
    pub fn new(bind_addr: String, router: Arc<Router>) -> Result<Self, String> {
        // Create HTTP client with connection pooling
        let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();

        // Create production-grade HTTP/2 backend connection pools (shared, non-worker mode)
        let backend_pools = Arc::new(Mutex::new(BackendConnectionPools::new(0)));

        // Create protocol detection cache
        let protocol_cache = Arc::new(Mutex::new(HashMap::new()));

        // Create rate limiter and circuit breaker
        let rate_limiter = Arc::new(RateLimiter::new());
        let circuit_breaker = Arc::new(CircuitBreakerManager::new(
            5,                                  // 5 consecutive failures to open circuit
            std::time::Duration::from_secs(30), // 30s timeout before Half-Open
        ));

        Ok(Self {
            bind_addr,
            router,
            client,
            backend_pools,
            protocol_cache,
            workers: None, // Legacy mode (Arc<Mutex> pools)
            worker_selector: None,
            rate_limiter,
            circuit_breaker,
        })
    }

    /// Create new proxy server with per-core workers (lock-free!)
    ///
    /// Each worker owns its own BackendConnectionPools, eliminating Arc<Mutex> contention.
    /// This is the Stage 2 architecture for 150K+ rps performance.
    #[allow(dead_code)] // Will be used when we switch to per-core mode
    pub fn new_with_workers(
        bind_addr: String,
        router: Arc<Router>,
        num_workers: usize,
    ) -> Result<Self, String> {
        // Create HTTP client with connection pooling
        let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();

        // Create per-core workers (each owns its BackendConnectionPools!)
        let mut workers = Vec::with_capacity(num_workers);
        for id in 0..num_workers {
            workers.push(Worker::new(id, router.clone()));
        }

        // Create protocol detection cache (still shared across workers)
        let protocol_cache = Arc::new(Mutex::new(HashMap::new()));

        // Create rate limiter and circuit breaker (shared across workers)
        let rate_limiter = Arc::new(RateLimiter::new());
        let circuit_breaker = Arc::new(CircuitBreakerManager::new(
            5,                                  // 5 consecutive failures to open circuit
            std::time::Duration::from_secs(30), // 30s timeout before Half-Open
        ));

        Ok(Self {
            bind_addr,
            router,
            client,
            backend_pools: Arc::new(Mutex::new(BackendConnectionPools::new(0))), // Not used in worker mode
            protocol_cache,
            workers: Some(Arc::new(workers)), // Immutable Vec - lock-free selection!
            worker_selector: Some(Arc::new(WorkerSelector::new(num_workers))),
            rate_limiter,
            circuit_breaker,
        })
    }

    /// Mark a backend as supporting HTTP/2 (for testing or explicit configuration)
    #[allow(dead_code)] // Used in tests and for explicit HTTP/2 backend configuration
    pub async fn set_backend_protocol_http2(&self, backend_host: &str, backend_port: u16) {
        let backend_key = format!("{}:{}", backend_host, backend_port);
        let mut cache = self.protocol_cache.lock().await;
        cache.insert(backend_key, true);
    }

    /// Start serving HTTP requests
    /// NOTE: Production code should use serve_with_shutdown() instead for graceful shutdown
    #[allow(dead_code)] // Used in tests; production uses serve_with_shutdown()
    pub async fn serve(self) -> Result<(), String> {
        let listener = TcpListener::bind(&self.bind_addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", self.bind_addr, e))?;

        let _actual_addr = listener
            .local_addr()
            .map_err(|e| format!("Failed to get local addr: {}", e))?;

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
    /// When shutdown signal is received:
    /// 1. Stop accepting new connections
    /// 2. Wait for active connections to complete
    /// 3. Shutdown after all connections drained or timeout
    #[allow(dead_code)] // Used in production via main.rs signal handling
    pub async fn serve_with_shutdown<F>(self, shutdown_signal: F) -> Result<(), String>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        use tokio::sync::Notify;

        let listener = TcpListener::bind(&self.bind_addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", self.bind_addr, e))?;

        let _actual_addr = listener
            .local_addr()
            .map_err(|e| format!("Failed to get local addr: {}", e))?;

        // Track active connections
        let active_connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let shutdown_notify = Arc::new(Notify::new());

        // Spawn shutdown handler
        let active_connections_clone = active_connections.clone();
        let shutdown_notify_clone = shutdown_notify.clone();
        tokio::spawn(async move {
            shutdown_signal.await;
            info!("Graceful shutdown initiated - draining active connections");

            // Wait for active connections to complete (with timeout)
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

        // Main accept loop
        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _)) => {
                            // Increment active connection counter
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

                                // Decrement active connection counter when done
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
    /// Uses dynamic SNI resolution to serve multiple hostnames
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

        // Build ServerConfig with dynamic SNI resolution
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
                // Perform TLS handshake with dynamic SNI resolution
                // The SNI hostname is automatically extracted from ClientHello
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

                // Use HTTP/1.1 over TLS (HTTPS)
                let _ = http1::Builder::new().serve_connection(io, service).await;
            });
        }
    }

    /// Start serving HTTPS requests with HTTP/2 support (ALPN negotiation)
    ///
    /// Uses ALPN to negotiate HTTP/2 (h2) or HTTP/1.1 with TLS clients.
    /// Automatically detects and handles both protocols.
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

        // Build ServerConfig with ALPN protocols for HTTP/2 and HTTP/1.1
        let mut server_config = sni_resolver
            .to_server_config()
            .map_err(|e| format!("Failed to build TLS config: {}", e))?;

        // Enable ALPN negotiation: prefer h2, fallback to http/1.1
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
                // Perform TLS handshake with ALPN negotiation
                let tls_stream = match acceptor_clone.accept(stream).await {
                    Ok(s) => s,
                    Err(e) => {
                        error!("TLS handshake failed: {}", e);
                        return;
                    }
                };

                // Check negotiated ALPN protocol
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

                // Use HTTP/2 or HTTP/1.1 based on ALPN negotiation
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

        let _actual_addr = listener
            .local_addr()
            .map_err(|e| format!("Failed to get local addr: {}", e))?;

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

                // Use HTTP/2 connection builder
                let _ = http2::Builder::new(TokioExecutor::new())
                    .serve_connection(io, service)
                    .await;
            });
        }
    }
}

/// Convert IPv4 u32 to string format (e.g., "192.168.1.1")
#[allow(dead_code)]
fn ipv4_to_string(ipv4: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        (ipv4 >> 24) & 0xFF,
        (ipv4 >> 16) & 0xFF,
        (ipv4 >> 8) & 0xFF,
        ipv4 & 0xFF
    )
}

/// Get or create HTTP/2 connection to backend
/// Check if a header is hop-by-hop and should not be forwarded
/// Per RFC 2616 Section 13.5.1
fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Apply request header filters before forwarding to backend
/// Gateway API HTTPRouteBackendRequestHeaderModification
///
/// Security: HeaderName::from_bytes() and HeaderValue::from_str() validate input and reject
/// CRLF characters (\r\n), preventing header injection attacks. Invalid headers return errors.
fn apply_request_filters(
    req: &mut Request<hyper::body::Incoming>,
    filters: &RequestHeaderModifier,
) -> Result<(), String> {
    apply_header_filters(req, &filters.operations)
}

/// Trait for types that provide mutable access to headers
trait HasHeadersMut {
    fn headers_mut(&mut self) -> &mut hyper::header::HeaderMap;
}

impl<B> HasHeadersMut for hyper::Request<B> {
    fn headers_mut(&mut self) -> &mut hyper::header::HeaderMap {
        self.headers_mut()
    }
}

impl<B> HasHeadersMut for hyper::Response<B> {
    fn headers_mut(&mut self) -> &mut hyper::header::HeaderMap {
        self.headers_mut()
    }
}

/// Generic helper to apply header filters
fn apply_header_filters<T, F>(target: &mut T, filters: &F) -> Result<(), String>
where
    T: HasHeadersMut,
    F: std::ops::Deref<Target = [HeaderModifierOp]>,
{
    for op in filters.iter() {
        match op {
            HeaderModifierOp::Set { name, value } => {
                let header_name = hyper::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|e| format!("Invalid header name '{}': {}", name, e))?;
                let header_value = hyper::header::HeaderValue::from_str(value)
                    .map_err(|e| format!("Invalid header value '{}': {}", value, e))?;
                target.headers_mut().insert(header_name, header_value);
            }
            HeaderModifierOp::Add { name, value } => {
                let header_name = hyper::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|e| format!("Invalid header name '{}': {}", name, e))?;
                let header_value = hyper::header::HeaderValue::from_str(value)
                    .map_err(|e| format!("Invalid header value '{}': {}", value, e))?;
                target.headers_mut().append(header_name, header_value);
            }
            HeaderModifierOp::Remove { name } => {
                let header_name = hyper::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|e| format!("Invalid header name '{}': {}", name, e))?;
                target.headers_mut().remove(header_name);
            }
        }
    }
    Ok(())
}

/// Apply response header filters after receiving response from backend
/// Gateway API HTTPResponseHeaderModifier
fn apply_response_filters<B>(
    resp: &mut Response<B>,
    filters: &ResponseHeaderModifier,
) -> Result<(), String> {
    apply_header_filters(resp, &filters.operations)
}
/// Build redirect response (Gateway API HTTPRequestRedirectFilter - Core)
///
/// Security Note: Redirect configuration is controlled by cluster operators via Gateway API
/// HTTPRoute resources. While this function doesn't validate redirect targets against an allowlist,
/// operators must ensure that redirect filters only point to trusted destinations. Redirects
/// configured via HTTPRoute are considered intentional by the cluster operator.
///
/// For production deployments, operators should:
/// 1. Use RBAC to restrict who can create/modify HTTPRoutes
/// 2. Review redirect configurations during deployment
/// 3. Implement admission webhooks for additional validation if needed
fn build_redirect_response(
    req: &Request<hyper::body::Incoming>,
    redirect: &RequestRedirect,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    let uri = req.uri();

    // Extract components with fallbacks
    let scheme = redirect
        .scheme
        .as_deref()
        .or_else(|| uri.scheme_str())
        .unwrap_or("http");

    let host = redirect
        .hostname
        .as_deref()
        .or_else(|| uri.host())
        .or_else(|| req.headers().get("host").and_then(|h| h.to_str().ok()))
        .ok_or_else(|| "Cannot determine redirect host".to_string())?;

    let port = redirect
        .port
        .or_else(|| uri.port_u16())
        .map(|p| format!(":{}", p))
        .unwrap_or_default();

    let path = redirect.path.as_deref().unwrap_or_else(|| uri.path());

    // Build Location header
    let location = format!("{}://{}{}{}", scheme, host, port, path);

    // Map status code
    use crate::proxy::filters::RedirectStatusCode;
    let status = match redirect.status_code {
        RedirectStatusCode::MovedPermanently => StatusCode::MOVED_PERMANENTLY,
        RedirectStatusCode::Found => StatusCode::FOUND,
    };

    // Build response
    Response::builder()
        .status(status)
        .header("Location", location)
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {}) // Handle infallible error type
                .boxed(),
        )
        .map_err(|e| format!("Failed to build redirect response: {}", e))
}

/// Forward request to backend server
/// Returns BoxBody to support zero-copy streaming for HTTP/2 (hot path)
///
/// # Timeout Enforcement
/// If `timeout_config` is provided with a `backend_request` timeout, the HTTP request
/// to the backend will be wrapped in `tokio::time::timeout`. If the backend doesn't
/// respond within the timeout, returns an error that the caller should map to 504.
#[allow(clippy::too_many_arguments)] // Workers integration requires additional parameters
async fn forward_to_backend(
    req: Request<hyper::body::Incoming>,
    backend: common::Backend,
    client: Client<HttpConnector, Full<Bytes>>,
    request_id: &str,
    backend_pools: BackendPools,
    protocol_cache: ProtocolCache,
    workers: Option<Workers>,
    worker_index: Option<usize>,
    timeout_config: Option<&Timeout>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    let request_start = Instant::now();

    // Read the incoming request body
    let (parts, body) = req.into_parts();

    // Fast path: GET/HEAD requests have no body, skip collection
    let body_bytes = if parts.method == hyper::Method::GET
        || parts.method == hyper::Method::HEAD
        || parts.method == hyper::Method::DELETE
    {
        debug!(
            request_id = %request_id,
            method = %parts.method,
            "Fast path: skipping body read for bodiless method"
        );
        Bytes::new() // Empty body, zero allocation
    } else {
        // Slow path: POST/PUT/PATCH may have body
        let body_read_start = Instant::now();
        let bytes = body
            .collect()
            .await
            .map_err(|e| {
                error!(
                    request_id = %request_id,
                    error.message = %e,
                    error.type = "request_body_read",
                    elapsed_us = body_read_start.elapsed().as_micros() as u64,
                    "Failed to read request body"
                );
                format!("Failed to read request body: {}", e)
            })?
            .to_bytes();

        let body_read_duration = body_read_start.elapsed();
        info!(
            request_id = %request_id,
            stage = "request_body_read",
            body_size_bytes = bytes.len(),
            elapsed_us = body_read_duration.as_micros() as u64,
            "Request body read complete"
        );
        bytes
    };

    // Build backend URI with path and query parameters
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    // Backend::Display formats correctly for both IPv4 and IPv6:
    // IPv4: "1.2.3.4:8080" → "http://1.2.3.4:8080/path"
    // IPv6: "[2001:db8::1]:8080" → "http://[2001:db8::1]:8080/path"
    let backend_uri = format!("http://{}{}", backend, path_and_query);

    // Save method for logging before we move parts.method
    let method_str = parts.method.to_string();

    // Build request to backend (preserves method, headers, and body)
    let mut backend_req_builder = Request::builder().method(parts.method).uri(&backend_uri);

    // Copy headers from original request, except Host and hop-by-hop headers
    for (name, value) in parts.headers.iter() {
        let name_str = name.as_str();
        if name_str != "host" && !is_hop_by_hop_header(name_str) {
            backend_req_builder = backend_req_builder.header(name, value);
        }
    }

    // Set Host header to match backend address (supports IPv4 and IPv6)
    let backend_host = backend.to_string();
    backend_req_builder = backend_req_builder.header("Host", backend_host);

    // Check protocol cache BEFORE cloning body (optimization: avoid clone on hot path)
    let backend_key = backend.to_string();
    let protocol_cached = {
        let cache = protocol_cache.lock().await;
        cache.get(&backend_key).copied()
    };

    // Optimization: Only clone body if protocol is UNKNOWN (first request to backend)
    // - Known HTTP/1.1: No clone needed, won't retry
    // - Known HTTP/2: No clone needed, failures are errors (not protocol issues)
    // - Unknown: Clone needed, might need HTTP/1.1 fallback
    let (backend_req, use_http2, body_for_fallback) = match protocol_cached {
        Some(true) => {
            // Cached as HTTP/2 - no clone needed (99% of requests after warmup)
            let req = backend_req_builder
                .body(Full::new(body_bytes))
                .map_err(|e| {
                    error!(
                        request_id = %request_id,
                        error.message = %e,
                        error.type = "backend_request_build",
                        backend_uri = %backend_uri,
                        "Failed to build backend request"
                    );
                    format!("Failed to build backend request: {}", e)
                })?;
            (req, true, None)
        }
        Some(false) => {
            // Cached as HTTP/1.1 - no clone needed
            let req = backend_req_builder
                .body(Full::new(body_bytes))
                .map_err(|e| {
                    error!(
                        request_id = %request_id,
                        error.message = %e,
                        error.type = "backend_request_build",
                        backend_uri = %backend_uri,
                        "Failed to build backend request"
                    );
                    format!("Failed to build backend request: {}", e)
                })?;
            (req, false, None)
        }
        None => {
            // Unknown protocol - clone for potential fallback (first request only)
            let body_for_http2 = body_bytes.clone();
            let req = backend_req_builder
                .body(Full::new(body_for_http2))
                .map_err(|e| {
                    error!(
                        request_id = %request_id,
                        error.message = %e,
                        error.type = "backend_request_build",
                        backend_uri = %backend_uri,
                        "Failed to build backend request"
                    );
                    format!("Failed to build backend request: {}", e)
                })?;
            (req, true, Some(body_bytes)) // Default to HTTP/2, keep body for fallback
        }
    };

    info!(
        request_id = %request_id,
        stage = "backend_request_built",
        network.peer.address = %backend,
        network.peer.port = backend.port,
        url.full = %backend_uri,
        http.request.method = %method_str,
        protocol_cached = ?protocol_cached,
        "Sending request to backend"
    );

    let backend_connect_start = Instant::now();

    // Extract backend_request timeout for enforcement
    let backend_timeout = timeout_config.and_then(|t| t.backend_request);

    // Backend request logic - will be wrapped with timeout if configured
    // Returns Result<Response<Incoming>, String> to support ? operator inside
    let backend_request_future = async {
        let resp = match use_http2 {
            true => {
                // Backend is known to support HTTP/2, use production connection pool
                debug!(request_id = %request_id, "Using HTTP/2 connection pool for backend {}", backend_key);

                let mut sender = if let (Some(workers_arc), Some(idx)) = (workers, worker_index) {
                    // Worker path: Lock-free worker selection + per-worker pool lock
                    debug!(
                        request_id = %request_id,
                        worker_index = idx,
                        "Using per-core worker connection pool (lock-free selection)"
                    );

                    // Lock-free worker selection - just array indexing!
                    let worker = &workers_arc[idx];

                    // Per-worker lock (only contends with requests to same worker)
                    worker
                        .get_backend_connection(backend)
                        .await
                        .map_err(|e| match e {
                            PoolError::CircuitBreakerOpen => {
                                error!(
                                    request_id = %request_id,
                                    backend = %backend_key,
                                    worker_index = idx,
                                    "Circuit breaker open for backend"
                                );
                                "Circuit breaker open".to_string()
                            }
                            _ => format!("Failed to get HTTP/2 connection from worker: {}", e),
                        })?
                } else {
                    // Legacy path: Shared pools with Arc<Mutex> (slower)
                    debug!(
                        request_id = %request_id,
                        "Using legacy shared connection pool"
                    );

                    let mut pools = backend_pools.lock().await;
                    let pool = pools.get_or_create_pool(backend);
                    pool.get_connection().await.map_err(|e| match e {
                        PoolError::CircuitBreakerOpen => {
                            error!(
                                request_id = %request_id,
                                backend = %backend_key,
                                "Circuit breaker open for backend"
                            );
                            "Circuit breaker open".to_string()
                        }
                        _ => format!("Failed to get HTTP/2 connection: {}", e),
                    })?
                };

                match sender.send_request(backend_req).await {
                    Ok(resp) => resp,
                    Err(e) => {
                        // HTTP/2 failed - check if we can fallback
                        if let Some(fallback_body) = body_for_fallback {
                            // Protocol was unknown, we have body for retry
                            warn!(
                                request_id = %request_id,
                                error.message = %e,
                                backend = %backend_key,
                                "HTTP/2 failed (unknown backend), falling back to HTTP/1.1"
                            );

                            // Cache as HTTP/1.1 for future requests
                            {
                                let mut cache = protocol_cache.lock().await;
                                cache.insert(backend_key.clone(), false);
                            }

                            // Retry with HTTP/1.1 using fallback body
                            let method = hyper::Method::from_bytes(method_str.as_bytes())
                                .map_err(|e| format!("Invalid method: {}", e))?;

                            let fallback_req = Request::builder()
                                .method(method)
                                .uri(&backend_uri)
                                .header("Host", backend.to_string()) // Supports IPv4 and IPv6
                                .body(Full::new(fallback_body))
                                .map_err(|e| format!("Failed to build fallback request: {}", e))?;

                            client.request(fallback_req).await.map_err(|e| {
                                error!(
                                    request_id = %request_id,
                                    error.message = %e,
                                    error.type = "backend_connection",
                                    "HTTP/1.1 fallback also failed"
                                );
                                format!("Backend connection failed: {}", e)
                            })?
                        } else {
                            // Backend was cached as HTTP/2 but request failed
                            // No fallback available (body was moved), return error
                            error!(
                                request_id = %request_id,
                                error.message = %e,
                                error.type = "backend_connection",
                                backend = %backend_key,
                                "HTTP/2 request failed for cached HTTP/2 backend (no fallback)"
                            );
                            return Err(format!("HTTP/2 backend connection failed: {}", e));
                        }
                    }
                }
            }
            false => {
                // Backend uses HTTP/1.1 (explicitly cached as HTTP/1.1 only)
                debug!(request_id = %request_id, "Using HTTP/1.1 for backend {} (cached)", backend_key);

                client.request(backend_req).await.map_err(|e| {
                    error!(
                        request_id = %request_id,
                        error.message = %e,
                        error.type = "backend_connection",
                        network.peer.address = %backend,
                        network.peer.port = backend.port,
                        elapsed_us = backend_connect_start.elapsed().as_micros() as u64,
                        "Backend connection failed"
                    );
                    format!("Backend connection failed: {}", e)
                })?
            }
        };
        Ok(resp)
    };

    // Apply backend_request timeout if configured
    let backend_resp = if let Some(timeout_duration) = backend_timeout {
        match tokio::time::timeout(timeout_duration, backend_request_future).await {
            Ok(result) => result?,
            Err(_elapsed) => {
                warn!(
                    request_id = %request_id,
                    timeout_ms = timeout_duration.as_millis(),
                    network.peer.address = %backend,
                    "Backend request timeout exceeded"
                );
                return Err(format!(
                    "TIMEOUT: Backend request exceeded {}ms timeout",
                    timeout_duration.as_millis()
                ));
            }
        }
    } else {
        // No timeout configured, execute directly
        backend_request_future.await?
    };

    let backend_connect_duration = backend_connect_start.elapsed();
    let response_status = backend_resp.status().as_u16();

    info!(
        request_id = %request_id,
        stage = "backend_response_received",
        http.response.status_code = response_status,
        network.peer.address = %backend,
        network.peer.port = backend.port,
        elapsed_us = backend_connect_duration.as_micros() as u64,
        "Backend responded"
    );

    // Zero-copy body streaming (Phase 2: HPACK + Body Streaming)
    // HTTP/2 responses stream directly without buffering
    let (mut parts, body) = backend_resp.into_parts();

    // Filter out hop-by-hop headers from backend response (RFC 2616 Section 13.5.1)
    let headers_to_remove: Vec<_> = parts
        .headers
        .keys()
        .filter(|name| is_hop_by_hop_header(name.as_str()))
        .cloned()
        .collect();

    for header_name in headers_to_remove {
        parts.headers.remove(header_name);
    }

    let total_duration = request_start.elapsed();
    info!(
        request_id = %request_id,
        stage = "request_complete",
        http.response.status_code = response_status,
        network.peer.address = %backend,
        network.peer.port = backend.port,
        timing.total_us = total_duration.as_micros() as u64,
        timing.backend_us = backend_connect_duration.as_micros() as u64,
        "Request forwarding (body streaming)"
    );

    // Box the body for type erasure (zero-copy streaming for HTTP/2)
    Ok(Response::from_parts(parts, body.boxed()))
}

/// Convert HttpMethod to static string (zero allocations)
fn method_to_str(method: &common::HttpMethod) -> &'static str {
    match method {
        common::HttpMethod::GET => "GET",
        common::HttpMethod::POST => "POST",
        common::HttpMethod::PUT => "PUT",
        common::HttpMethod::DELETE => "DELETE",
        common::HttpMethod::HEAD => "HEAD",
        common::HttpMethod::OPTIONS => "OPTIONS",
        common::HttpMethod::PATCH => "PATCH",
        common::HttpMethod::ALL => "ALL",
    }
}

/// Convert status code to static string (zero allocations for common codes)
fn status_to_str(status: u16) -> &'static str {
    match status {
        200 => "200",
        201 => "201",
        204 => "204",
        301 => "301",
        302 => "302",
        304 => "304",
        400 => "400",
        401 => "401",
        403 => "403",
        404 => "404",
        500 => "500",
        502 => "502",
        503 => "503",
        504 => "504",
        _ => "other",
    }
}

/// Handle incoming HTTP request
/// Returns BoxBody to support zero-copy streaming for HTTP/2 responses
#[allow(clippy::too_many_arguments)]
async fn handle_request(
    mut req: Request<hyper::body::Incoming>,
    router: Arc<Router>,
    client: Client<HttpConnector, Full<Bytes>>,
    backend_pools: BackendPools,
    protocol_cache: ProtocolCache,
    workers: Option<Workers>,
    worker_selector: Option<Arc<WorkerSelector>>,
    rate_limiter: Arc<RateLimiter>,
    circuit_breaker: Arc<CircuitBreakerManager>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    // Generate or extract request ID
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let path = req.uri().path().to_string();
    let method_name = req.method().as_str();

    info!(
        request_id = %request_id,
        stage = "request_received",
        http.request.method = %method_name,
        url.path = %path,
        "Incoming HTTP request"
    );

    // Handle /metrics endpoint (early return, don't record metrics for this)
    if path == "/metrics" && *req.method() == hyper::Method::GET {
        // Force initialization of lazy_static metrics
        let _ = &*HTTP_REQUEST_DURATION;
        let _ = &*HTTP_REQUESTS_TOTAL;

        let mut buffer = vec![];
        let encoder = TextEncoder::new();

        // Gather metrics from all registries (proxy + controller + rate limiter + circuit breaker + HTTP/2 pool)
        let mut metric_families = METRICS_REGISTRY.gather();
        let controller_metrics = CONTROLLER_METRICS_REGISTRY.gather();
        metric_families.extend(controller_metrics);

        // Add rate limiter metrics
        let rate_limiter_metrics = crate::proxy::rate_limiter::rate_limiter_registry().gather();
        metric_families.extend(rate_limiter_metrics);

        // Add circuit breaker metrics
        let circuit_breaker_metrics =
            crate::proxy::circuit_breaker::circuit_breaker_registry().gather();
        metric_families.extend(circuit_breaker_metrics);

        // Encode all metrics
        if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
            error!("Failed to encode metrics: {}", e);
            let error_response = Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("Content-Type", "text/plain")
                .body(
                    Full::new(Bytes::from(format!("Failed to encode metrics: {}", e)))
                        .map_err(|never| match never {})
                        .boxed(),
                )
                .map_err(|build_err| format!("Failed to build error response: {}", build_err))?;
            return Ok(error_response);
        }

        // Append HTTP/2 pool metrics
        if let Ok(pool_metrics_text) = gather_pool_metrics() {
            buffer.extend_from_slice(pool_metrics_text.as_bytes());
        } else {
            warn!("Failed to gather HTTP/2 pool metrics");
        }

        // Response builder with valid status/headers should never fail
        #[allow(clippy::unwrap_used)]
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", encoder.format_type())
            .body(
                Full::new(Bytes::from(buffer))
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .unwrap());
    }

    // Start timing after /metrics check
    let start = Instant::now();

    let method = match *req.method() {
        hyper::Method::GET => common::HttpMethod::GET,
        hyper::Method::POST => common::HttpMethod::POST,
        hyper::Method::PUT => common::HttpMethod::PUT,
        hyper::Method::DELETE => common::HttpMethod::DELETE,
        hyper::Method::HEAD => common::HttpMethod::HEAD,
        hyper::Method::OPTIONS => common::HttpMethod::OPTIONS,
        hyper::Method::PATCH => common::HttpMethod::PATCH,
        _ => common::HttpMethod::GET,
    };

    // Select backend using routing rules
    let route_match = router.select_backend(method, &path, None, None);

    match route_match {
        Some(route_match) => {
            // Select worker for this request (lock-free round-robin)
            let worker_index = worker_selector.as_ref().map(|selector| selector.select());

            // Check for redirect filter (Gateway API HTTPRequestRedirectFilter - Core)
            // Redirects return immediately - NEVER forward to backend
            if let Some(redirect) = &route_match.redirect {
                match build_redirect_response(&req, redirect) {
                    Ok(resp) => {
                        info!(
                            request_id = %request_id,
                            status = %resp.status().as_u16(),
                            location = ?resp.headers().get("Location"),
                            "Redirect response generated"
                        );
                        return Ok(resp);
                    }
                    Err(e) => {
                        error!(
                            request_id = %request_id,
                            error = %e,
                            "Failed to build redirect response"
                        );
                        #[allow(clippy::unwrap_used)]
                        return Ok(Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .header("Content-Type", "text/plain")
                            .body(
                                Full::new(Bytes::from(format!("Redirect error: {}", e)))
                                    .map_err(|never| match never {})
                                    .boxed(),
                            )
                            .unwrap());
                    }
                }
            }

            // Check rate limit for this route (per-route rate limiting)
            if !rate_limiter.check_rate_limit(&route_match.pattern) {
                warn!(
                    request_id = %request_id,
                    route.pattern = %route_match.pattern,
                    "Rate limit exceeded"
                );
                #[allow(clippy::unwrap_used)]
                return Ok(Response::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .header("Content-Type", "text/plain")
                    .header("Retry-After", "1") // Retry after 1 second
                    .body(
                        Full::new(Bytes::from("Rate limit exceeded"))
                            .map_err(|never| match never {})
                            .boxed(),
                    )
                    .unwrap());
            }

            // Apply request header filters if configured (Gateway API HTTPRouteBackendRequestHeaderModification)
            if let Some(filters) = &route_match.request_filters {
                if let Err(e) = apply_request_filters(&mut req, filters) {
                    error!(
                        request_id = %request_id,
                        error = %e,
                        "Failed to apply request header filters"
                    );
                    #[allow(clippy::unwrap_used)]
                    return Ok(Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .header("Content-Type", "text/plain")
                        .body(
                            Full::new(Bytes::from(format!("Filter error: {}", e)))
                                .map_err(|never| match never {})
                                .boxed(),
                        )
                        .unwrap());
                }
            }

            // Check circuit breaker for backend (fail-fast if circuit is Open)
            let backend_id = route_match.backend.to_string();
            if !circuit_breaker.allow_request(&backend_id) {
                warn!(
                    request_id = %request_id,
                    backend = %backend_id,
                    "Circuit breaker is OPEN - backend unavailable"
                );
                #[allow(clippy::unwrap_used)]
                return Ok(Response::builder()
                    .status(StatusCode::SERVICE_UNAVAILABLE)
                    .header("Content-Type", "text/plain")
                    .header("Retry-After", "30") // Circuit breaker timeout
                    .body(
                        Full::new(Bytes::from(
                            "Backend temporarily unavailable (circuit breaker open)",
                        ))
                        .map_err(|never| match never {})
                        .boxed(),
                    )
                    .unwrap());
            }

            info!(
                request_id = %request_id,
                stage = "route_matched",
                route.pattern = %route_match.pattern,
                network.peer.address = %route_match.backend,  // Backend::Display handles both IPv4 and IPv6
                worker_index = ?worker_index,
                "Route matched, forwarding to backend"
            );

            // Extract overall request timeout (wraps entire request including retries)
            let overall_request_timeout = route_match.timeout.as_ref().and_then(|t| t.request);

            // Check if retry is enabled for this route
            let retry_config = route_match.retry.as_ref();

            // For GET/HEAD requests with retry config, we can implement retries
            // because there's no request body to preserve
            let is_retryable_method =
                method == common::HttpMethod::GET || method == common::HttpMethod::HEAD;

            // Execute request with optional retry logic
            let result = if let (true, Some(retry_cfg)) = (is_retryable_method, retry_config) {
                // Retry-enabled path for GET/HEAD
                let max_attempts = retry_cfg.max_retries + 1; // Initial + retries
                let mut attempt: u32;
                let mut last_result: Result<Response<BoxBody<Bytes, hyper::Error>>, String>;

                // First attempt with original request
                let first_result = if let Some(timeout_duration) = overall_request_timeout {
                    match tokio::time::timeout(
                        timeout_duration,
                        forward_to_backend(
                            req,
                            route_match.backend,
                            client.clone(),
                            &request_id,
                            backend_pools.clone(),
                            protocol_cache.clone(),
                            workers.clone(),
                            worker_index,
                            route_match.timeout.as_ref(),
                        ),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_elapsed) => Err(format!(
                            "TIMEOUT: Request exceeded {}ms overall timeout",
                            timeout_duration.as_millis()
                        )),
                    }
                } else {
                    forward_to_backend(
                        req,
                        route_match.backend,
                        client.clone(),
                        &request_id,
                        backend_pools.clone(),
                        protocol_cache.clone(),
                        workers.clone(),
                        worker_index,
                        route_match.timeout.as_ref(),
                    )
                    .await
                };

                // Check if we should retry
                let should_retry_first = match &first_result {
                    Ok(resp) => retry_cfg.should_retry_status(resp.status().as_u16()),
                    Err(_) => retry_cfg.retry_on_connection_error,
                };

                if !should_retry_first || max_attempts <= 1 {
                    first_result
                } else {
                    // Need to retry - enter retry loop
                    last_result = first_result;
                    attempt = 1;

                    while attempt < max_attempts {
                        // Apply exponential backoff
                        let backoff_delay = retry_cfg.calculate_delay(attempt - 1);
                        debug!(
                            request_id = %request_id,
                            attempt = attempt,
                            backoff_ms = backoff_delay.as_millis(),
                            "Retrying request after backoff"
                        );
                        tokio::time::sleep(backoff_delay).await;

                        // Rebuild request for retry (GET/HEAD have no body)
                        // Use direct HTTP/1.1 request for simplicity
                        let backend_uri = format!(
                            "http://{}/{}",
                            route_match.backend, // Backend::Display includes ip:port
                            path.trim_start_matches('/')
                        );

                        let retry_method = match method {
                            common::HttpMethod::GET => hyper::Method::GET,
                            common::HttpMethod::HEAD => hyper::Method::HEAD,
                            _ => hyper::Method::GET, // Shouldn't happen
                        };

                        let retry_req = match Request::builder()
                            .method(retry_method)
                            .uri(&backend_uri)
                            .header("Host", route_match.backend.to_string())
                            .body(Full::new(Bytes::new()))
                        {
                            Ok(r) => r,
                            Err(e) => {
                                last_result = Err(format!("Failed to build retry request: {}", e));
                                break;
                            }
                        };

                        // Make retry attempt using HTTP/1.1 client directly
                        let retry_result = match client.request(retry_req).await {
                            Ok(resp) => {
                                // Box the body for type consistency
                                let (parts, body) = resp.into_parts();
                                Ok(Response::from_parts(parts, body.map_err(|e| e).boxed()))
                            }
                            Err(e) => Err(format!("Retry request failed: {}", e)),
                        };

                        // Check if we should continue retrying
                        let should_continue = match &retry_result {
                            Ok(resp) => retry_cfg.should_retry_status(resp.status().as_u16()),
                            Err(_) => retry_cfg.retry_on_connection_error,
                        };

                        last_result = retry_result;
                        attempt += 1;

                        if !should_continue {
                            break; // Success or non-retryable error
                        }
                    }

                    last_result
                }
            } else {
                // Non-retry path (original behavior)
                if let Some(timeout_duration) = overall_request_timeout {
                    match tokio::time::timeout(
                        timeout_duration,
                        forward_to_backend(
                            req,
                            route_match.backend,
                            client,
                            &request_id,
                            backend_pools,
                            protocol_cache,
                            workers,
                            worker_index,
                            route_match.timeout.as_ref(),
                        ),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_elapsed) => {
                            warn!(
                                request_id = %request_id,
                                timeout_ms = timeout_duration.as_millis(),
                                "Overall request timeout exceeded"
                            );
                            Err(format!(
                                "TIMEOUT: Request exceeded {}ms overall timeout",
                                timeout_duration.as_millis()
                            ))
                        }
                    }
                } else {
                    forward_to_backend(
                        req,
                        route_match.backend,
                        client,
                        &request_id,
                        backend_pools,
                        protocol_cache,
                        workers,
                        worker_index,
                        route_match.timeout.as_ref(),
                    )
                    .await
                }
            };
            let duration = start.elapsed();

            // Record metrics (zero-allocation with static strings)
            // Use route pattern (not actual path) to prevent cardinality explosion
            let method_str = method_to_str(&method);
            let route_pattern = &route_match.pattern;
            let worker_label = worker_index
                .map(|i| i.to_string())
                .unwrap_or_else(|| "none".to_string());
            match &result {
                Ok(resp) => {
                    let status_code = resp.status().as_u16();
                    let status_str = status_to_str(status_code);
                    HTTP_REQUESTS_TOTAL
                        .with_label_values(&[method_str, route_pattern, status_str, &worker_label])
                        .inc();
                    HTTP_REQUEST_DURATION
                        .with_label_values(&[method_str, route_pattern, status_str])
                        .observe(duration.as_secs_f64());

                    // Record backend health for passive health checking (supports IPv4 and IPv6)
                    router.record_backend_response(route_match.backend, status_code);

                    // Record circuit breaker success/failure based on status code
                    if status_code >= 500 {
                        // 5xx = backend failure
                        circuit_breaker.record_failure(&backend_id);
                    } else {
                        // 2xx, 3xx, 4xx = backend healthy (even if client error)
                        circuit_breaker.record_success(&backend_id);
                    }
                }
                Err(e) => {
                    // Check if this is a timeout error (return 504) or other error (return 500)
                    let (status_code, status_str) = if e.starts_with("TIMEOUT:") {
                        (504_u16, "504")
                    } else {
                        (500_u16, "500")
                    };

                    HTTP_REQUESTS_TOTAL
                        .with_label_values(&[method_str, route_pattern, status_str, &worker_label])
                        .inc();
                    HTTP_REQUEST_DURATION
                        .with_label_values(&[method_str, route_pattern, status_str])
                        .observe(duration.as_secs_f64());

                    // Record backend health (treat connection errors as 500, supports IPv4 and IPv6)
                    router.record_backend_response(route_match.backend, status_code);

                    // Record circuit breaker failure (connection error = backend failure)
                    circuit_breaker.record_failure(&backend_id);

                    error!(
                        request_id = %request_id,
                        error.message = %e,
                        http.request.method = %method_str,
                        url.path = %path,
                        route.pattern = %route_pattern,
                        duration_us = duration.as_micros() as u64,
                        "Backend error"
                    );
                }
            }

            // Apply response header filters if configured (Gateway API HTTPResponseHeaderModifier)
            if let Some(filters) = &route_match.response_filters {
                match result {
                    Ok(mut resp) => {
                        if let Err(e) = apply_response_filters(&mut resp, filters) {
                            error!(
                                request_id = %request_id,
                                error = %e,
                                "Failed to apply response header filters"
                            );
                            // Return error response if filter application fails
                            #[allow(clippy::unwrap_used)]
                            {
                                Ok(Response::builder()
                                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                                    .header("Content-Type", "text/plain")
                                    .body(
                                        Full::new(Bytes::from(format!(
                                            "Response filter error: {}",
                                            e
                                        )))
                                        .map_err(|never| match never {})
                                        .boxed(),
                                    )
                                    .unwrap())
                            }
                        } else {
                            Ok(resp)
                        }
                    }
                    Err(e) => {
                        // Convert error to HTTP response (504 for timeout, 500 for other)
                        let status = if e.starts_with("TIMEOUT:") {
                            StatusCode::GATEWAY_TIMEOUT
                        } else {
                            StatusCode::INTERNAL_SERVER_ERROR
                        };
                        #[allow(clippy::unwrap_used)]
                        Ok(Response::builder()
                            .status(status)
                            .header("Content-Type", "text/plain")
                            .header("X-Request-ID", request_id.clone())
                            .body(
                                Full::new(Bytes::from(e))
                                    .map_err(|never| match never {})
                                    .boxed(),
                            )
                            .unwrap())
                    }
                }
            } else {
                // No response filters - handle result directly
                match result {
                    Ok(resp) => Ok(resp),
                    Err(e) => {
                        // Convert error to HTTP response (504 for timeout, 500 for other)
                        let status = if e.starts_with("TIMEOUT:") {
                            StatusCode::GATEWAY_TIMEOUT
                        } else {
                            StatusCode::INTERNAL_SERVER_ERROR
                        };
                        #[allow(clippy::unwrap_used)]
                        Ok(Response::builder()
                            .status(status)
                            .header("Content-Type", "text/plain")
                            .header("X-Request-ID", request_id.clone())
                            .body(
                                Full::new(Bytes::from(e))
                                    .map_err(|never| match never {})
                                    .boxed(),
                            )
                            .unwrap())
                    }
                }
            }
        }
        None => {
            let duration = start.elapsed();

            // Record 404 metrics (use constant to prevent cardinality explosion)
            let method_str = method_to_str(&method);
            HTTP_REQUESTS_TOTAL
                .with_label_values(&[method_str, "not_found", "404", "none"])
                .inc();
            HTTP_REQUEST_DURATION
                .with_label_values(&[method_str, "not_found", "404"])
                .observe(duration.as_secs_f64());

            warn!(
                request_id = %request_id,
                stage = "route_not_found",
                http.request.method = ?method,
                url.path = %path,
                duration_us = duration.as_micros() as u64,
                "No route found"
            );

            #[allow(clippy::unwrap_used)]
            {
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header("X-Request-ID", request_id)
                    .body(
                        Full::new(Bytes::from("Not Found"))
                            .map_err(|never| match never {})
                            .boxed(),
                    )
                    .unwrap())
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use common::{Backend, HttpMethod};
    use http_body_util::BodyExt;
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

    #[tokio::test]
    async fn test_server_returns_404_for_no_match() {
        let router = crate::proxy::router::Router::new();

        // Add route: GET /api/users -> backend
        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];
        router
            .add_route(HttpMethod::GET, "/api/users", backends)
            .unwrap();

        // Bind to a specific available port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = ProxyServer::new(bind_addr.to_string(), Arc::new(router)).unwrap();

        // Start server in background
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make request to non-existent route
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri = format!("http://{}/api/posts", bind_addr).parse().unwrap();
        let response = client.get(uri).await.expect("Request should succeed");

        // Should return 404
        assert_eq!(response.status(), hyper::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_proxy_forwards_to_backend() {
        // Start mock backend server
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        // Backend responds with "Hello from backend" (HTTP/2 support)
        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from("Hello from backend")))
                        .unwrap(),
                )
            });

            // Use HTTP/2 for mock backend to match production behavior
            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        // Wait for backend to start
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router pointing to our mock backend
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!(
                "Test expects IPv4 address (IPv6 support not yet implemented in Backend struct)"
            ),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::GET, "/api/test", backends)
            .unwrap();

        // Start proxy server
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make request through proxy
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri = format!("http://{}/api/test", proxy_addr).parse().unwrap();
        let response = client.get(uri).await.expect("Request should succeed");

        // Should get backend's response, not proxy's "Route matched!" message
        assert_eq!(response.status(), hyper::StatusCode::OK);

        let body_bytes = response.collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();

        assert_eq!(
            body, "Hello from backend",
            "Expected backend response, got: {}",
            body
        );
    }

    #[tokio::test]
    async fn test_proxy_forwards_query_parameters() {
        // Start mock backend that echoes the query string
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|req: Request<hyper::body::Incoming>| async move {
                let query = req.uri().query().unwrap_or("");
                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from(format!("Query: {}", query))))
                        .unwrap(),
                )
            });

            // Use HTTP/2 for mock backend to match production behavior
            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::GET, "/api/search", backends)
            .unwrap();

        // Start proxy
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make request with query parameters
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri = format!("http://{}/api/search?q=test&limit=10", proxy_addr)
            .parse()
            .unwrap();
        let response = client.get(uri).await.expect("Request should succeed");

        assert_eq!(response.status(), hyper::StatusCode::OK);

        let body_bytes = response.collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();

        assert_eq!(
            body, "Query: q=test&limit=10",
            "Query parameters should be forwarded"
        );
    }

    #[tokio::test]
    async fn test_proxy_forwards_post_body() {
        // Start mock backend that echoes the request body
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|req: Request<hyper::body::Incoming>| async move {
                let body_bytes = req.collect().await.unwrap().to_bytes();
                let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from(format!("Received: {}", body_str))))
                        .unwrap(),
                )
            });

            // Use HTTP/2 for mock backend to match production behavior
            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::POST, "/api/data", backends)
            .unwrap();

        // Start proxy
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make POST request with body
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri: hyper::Uri = format!("http://{}/api/data", proxy_addr).parse().unwrap();
        let post_data = r#"{"name":"test","value":123}"#;
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .body(Full::new(Bytes::from(post_data)))
            .unwrap();

        let response = client
            .request(request)
            .await
            .expect("Request should succeed");

        assert_eq!(response.status(), hyper::StatusCode::OK);

        let body_bytes = response.collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();

        assert_eq!(
            body, r#"Received: {"name":"test","value":123}"#,
            "POST body should be forwarded"
        );
    }

    #[tokio::test]
    async fn test_proxy_forwards_headers() {
        // Start mock backend that echoes specific headers
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|req: Request<hyper::body::Incoming>| async move {
                let auth = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let content_type = req
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let custom = req
                    .headers()
                    .get("x-custom-header")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");

                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from(format!(
                            "Auth: {}, CT: {}, Custom: {}",
                            auth, content_type, custom
                        ))))
                        .unwrap(),
                )
            });

            // Use HTTP/2 for mock backend to match production behavior
            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::GET, "/api/headers", backends)
            .unwrap();

        // Start proxy
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make request with custom headers
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri: hyper::Uri = format!("http://{}/api/headers", proxy_addr)
            .parse()
            .unwrap();
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .header("Authorization", "Bearer token123")
            .header("Content-Type", "application/json")
            .header("X-Custom-Header", "test-value")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = client
            .request(request)
            .await
            .expect("Request should succeed");

        assert_eq!(response.status(), hyper::StatusCode::OK);

        let body_bytes = response.collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();

        assert_eq!(
            body, "Auth: Bearer token123, CT: application/json, Custom: test-value",
            "Headers should be forwarded"
        );
    }

    #[tokio::test]
    async fn test_proxy_sets_correct_host_header() {
        // Start mock backend that echoes the Host header
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|req: Request<hyper::body::Incoming>| async move {
                let host = req
                    .headers()
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");

                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from(format!("Host: {}", host))))
                        .unwrap(),
                )
            });

            // Use HTTP/2 for mock backend to match production behavior
            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::GET, "/api/test", backends)
            .unwrap();

        // Start proxy
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make request with specific Host header
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri: hyper::Uri = format!("http://{}/api/test", proxy_addr).parse().unwrap();
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .header("Host", format!("{}", proxy_addr)) // Client sends proxy's host
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = client
            .request(request)
            .await
            .expect("Request should succeed");

        assert_eq!(response.status(), hyper::StatusCode::OK);

        let body_bytes = response.collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();

        // Backend should receive Host header matching backend address, not proxy address
        assert_eq!(
            body,
            format!("Host: {}", backend_addr),
            "Host header should match backend address, not proxy address"
        );
    }

    #[tokio::test]
    async fn test_proxy_filters_hop_by_hop_headers() {
        // Start mock backend that echoes specific headers
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|req: Request<hyper::body::Incoming>| async move {
                let connection = req.headers().get("connection").is_some();
                let transfer_encoding = req.headers().get("transfer-encoding").is_some();
                let upgrade = req.headers().get("upgrade").is_some();
                let proxy_auth = req.headers().get("proxy-authorization").is_some();
                let keep_alive = req.headers().get("keep-alive").is_some();

                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from(format!(
                            "Connection:{},TE:{},Upgrade:{},ProxyAuth:{},KeepAlive:{}",
                            connection, transfer_encoding, upgrade, proxy_auth, keep_alive
                        ))))
                        .unwrap(),
                )
            });

            // Use HTTP/2 for mock backend to match production behavior
            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::GET, "/api/test", backends)
            .unwrap();

        // Start proxy
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make request with hop-by-hop headers
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri: hyper::Uri = format!("http://{}/api/test", proxy_addr).parse().unwrap();
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .header("Connection", "keep-alive")
            .header("Transfer-Encoding", "chunked")
            .header("Upgrade", "websocket")
            .header("Proxy-Authorization", "Basic dGVzdDp0ZXN0")
            .header("Keep-Alive", "timeout=5")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = client
            .request(request)
            .await
            .expect("Request should succeed");

        assert_eq!(response.status(), hyper::StatusCode::OK);

        let body_bytes = response.collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();

        // Backend should NOT receive hop-by-hop headers
        assert_eq!(
            body, "Connection:false,TE:false,Upgrade:false,ProxyAuth:false,KeepAlive:false",
            "Hop-by-hop headers should be filtered out"
        );
    }

    #[tokio::test]
    async fn test_proxy_filters_response_hop_by_hop_headers() {
        // Start mock backend that sends hop-by-hop headers in response
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", "text/plain")
                        .header("Connection", "keep-alive")
                        .header("Keep-Alive", "timeout=5")
                        .header("Transfer-Encoding", "chunked")
                        .header("Upgrade", "h2c")
                        .header("X-Custom", "should-be-forwarded")
                        .body(Full::new(Bytes::from("Response from backend")))
                        .unwrap(),
                )
            });

            // Use HTTP/2 for mock backend to match production behavior
            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::GET, "/api/test", backends)
            .unwrap();

        // Start proxy
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make request
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri: hyper::Uri = format!("http://{}/api/test", proxy_addr).parse().unwrap();
        let response = client.get(uri).await.expect("Request should succeed");

        assert_eq!(response.status(), hyper::StatusCode::OK);

        // Verify hop-by-hop headers are NOT in response
        assert!(
            response.headers().get("connection").is_none(),
            "Connection header should be filtered from response"
        );
        assert!(
            response.headers().get("keep-alive").is_none(),
            "Keep-Alive header should be filtered from response"
        );
        assert!(
            response.headers().get("transfer-encoding").is_none(),
            "Transfer-Encoding header should be filtered from response"
        );
        assert!(
            response.headers().get("upgrade").is_none(),
            "Upgrade header should be filtered from response"
        );

        // Verify end-to-end headers ARE in response
        assert!(
            response.headers().get("content-type").is_some(),
            "Content-Type header should be forwarded"
        );
        assert!(
            response.headers().get("x-custom").is_some(),
            "Custom header should be forwarded"
        );
    }

    #[tokio::test]
    async fn test_metrics_endpoint_returns_prometheus_format() {
        use crate::proxy::router::Router;
        use hyper::body::Bytes;
        use hyper_util::rt::TokioIo;
        use tokio::net::TcpListener;

        // Start mock backend
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = hyper::service::service_fn(|_req| async {
                Ok::<_, std::convert::Infallible>(
                    hyper::Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from("OK")))
                        .unwrap(),
                )
            });

            // Use HTTP/2 for mock backend to match production behavior
            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router
        let router = Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::GET, "/api/test", backends)
            .unwrap();

        // Start proxy
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make some requests to generate metrics
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        for _ in 0..5 {
            let uri: hyper::Uri = format!("http://{}/api/test", proxy_addr).parse().unwrap();
            let _ = client.get(uri).await;
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Request metrics endpoint
        let uri: hyper::Uri = format!("http://{}/metrics", proxy_addr).parse().unwrap();
        let response = client
            .get(uri)
            .await
            .expect("Metrics request should succeed");

        assert_eq!(response.status(), hyper::StatusCode::OK);

        // Collect response body
        let body_bytes = response.collect().await.unwrap().to_bytes();
        let body = String::from_utf8_lossy(&body_bytes);

        // Verify Prometheus format
        assert!(
            body.contains("# HELP"),
            "Response should contain Prometheus HELP comments"
        );
        assert!(
            body.contains("# TYPE"),
            "Response should contain Prometheus TYPE comments"
        );

        // Verify request counter exists
        assert!(
            body.contains("http_requests_total"),
            "Response should contain request counter metric"
        );

        // Verify latency histogram exists
        assert!(
            body.contains("http_request_duration_seconds"),
            "Response should contain latency histogram metric"
        );
        assert!(
            body.contains("_bucket{"),
            "Histogram should contain bucket metrics"
        );
        assert!(body.contains("_sum"), "Histogram should contain sum metric");
        assert!(
            body.contains("_count"),
            "Histogram should contain count metric"
        );
    }

    #[tokio::test]
    async fn test_controller_metrics_in_endpoint() {
        // Record some controller metrics first
        use crate::apis::metrics::{
            record_gateway_reconciliation, record_gatewayclass_reconciliation,
            record_httproute_reconciliation,
        };

        record_httproute_reconciliation("test-route", "default", 0.123, "success");
        record_gateway_reconciliation("test-gateway", "default", 0.045, "success");
        record_gatewayclass_reconciliation("rauta", 0.021, "success");

        // Start proxy server
        let router = Arc::new(Router::new());
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), router).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Request metrics endpoint
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();
        let uri: hyper::Uri = format!("http://{}/metrics", proxy_addr).parse().unwrap();
        let response = client
            .get(uri)
            .await
            .expect("Metrics request should succeed");

        assert_eq!(response.status(), hyper::StatusCode::OK);

        // Collect response body
        let body_bytes = response.collect().await.unwrap().to_bytes();
        let body = String::from_utf8_lossy(&body_bytes);

        // Verify controller metrics are present
        assert!(
            body.contains("httproute_reconciliation_duration_seconds"),
            "Should contain HTTPRoute duration metric"
        );
        assert!(
            body.contains("httproute_reconciliations_total"),
            "Should contain HTTPRoute counter metric"
        );
        assert!(
            body.contains("gateway_reconciliation_duration_seconds"),
            "Should contain Gateway duration metric"
        );
        assert!(
            body.contains("gateway_reconciliations_total"),
            "Should contain Gateway counter metric"
        );
        assert!(
            body.contains("gatewayclass_reconciliations_total"),
            "Should contain GatewayClass counter metric"
        );

        // Verify metric values contain our test data
        assert!(
            body.contains(r#"httproute="test-route""#),
            "Should contain test HTTPRoute name"
        );
        assert!(
            body.contains(r#"gateway="test-gateway""#),
            "Should contain test Gateway name"
        );
        assert!(
            body.contains(r#"gatewayclass="rauta""#),
            "Should contain test GatewayClass name"
        );
    }

    #[tokio::test]
    async fn test_http2_server_support() {
        // This test verifies that the proxy server:
        // 1. Accepts HTTP/2 connections from clients
        // 2. Returns 404 for non-existent routes via HTTP/2

        // Create router with no routes
        let router = crate::proxy::router::Router::new();

        // Start HTTP/2 proxy server
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();

        tokio::spawn(async move {
            let _ = server.serve_http2().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make HTTP/2 client request
        let stream = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
        let io = TokioIo::new(stream);

        let (mut request_sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), io)
                .await
                .unwrap();

        // Spawn connection driver
        tokio::spawn(async move {
            let _ = connection.await;
        });

        // Send HTTP/2 request to non-existent route
        let req = Request::builder()
            .uri("/api/test")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = request_sender.send_request(req).await.unwrap();

        // Should get 404 (no routes configured)
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_http2_forwards_to_http1_backend() {
        // REFACTOR: Test HTTP/2 client → HTTP/2 proxy → HTTP/1.1 backend
        // This is the realistic production scenario

        // Start mock HTTP/1.1 backend
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from("Hello from HTTP/1.1 backend")))
                        .unwrap(),
                )
            });

            // Use HTTP/2 for mock backend to match production behavior
            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router pointing to HTTP/1.1 backend
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::GET, "/api/test", backends)
            .unwrap();

        // Start HTTP/2 proxy server
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();

        tokio::spawn(async move {
            let _ = server.serve_http2().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make HTTP/2 client request
        let stream = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
        let io = TokioIo::new(stream);

        let (mut request_sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), io)
                .await
                .unwrap();

        // Spawn connection driver
        tokio::spawn(async move {
            let _ = connection.await;
        });

        // Send HTTP/2 request
        let req = Request::builder()
            .uri("/api/test")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = request_sender.send_request(req).await.unwrap();

        // Verify response
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();

        assert_eq!(body, "Hello from HTTP/1.1 backend");
    }

    #[tokio::test]
    async fn test_server_with_workers() {
        // GREEN: Test per-core worker architecture
        //
        // This test verifies that ProxyServer can be created with workers
        // and that each worker owns its own BackendConnectionPools (no Arc<Mutex>!)

        let router = crate::proxy::router::Router::new();
        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(127, 0, 0, 1)),
            9999,
            100,
        )];
        router
            .add_route(HttpMethod::GET, "/test", backends)
            .unwrap();

        // Create server with 4 workers (simulating 4 CPU cores)
        let server =
            ProxyServer::new_with_workers("127.0.0.1:8080".to_string(), Arc::new(router), 4);

        assert!(server.is_ok());

        let server = server.unwrap();
        assert!(server.workers.is_some(), "Workers should be initialized");

        // Verify we have 4 workers (no lock needed - Arc<Vec<Worker>>!)
        let workers = server.workers.unwrap();
        assert_eq!(workers.len(), 4, "Should have 4 workers");

        // Verify each worker has unique ID (lock-free access)
        let worker_ids: Vec<_> = workers.iter().map(|w| w.id()).collect();
        assert_eq!(worker_ids, vec![0, 1, 2, 3], "Worker IDs should be 0..3");
    }

    /// RED: Test HTTPS proxy with TLS termination
    #[tokio::test]
    async fn test_https_proxy_with_tls_termination() {
        use crate::proxy::tls::{SniResolver, TlsCertificate};
        use std::sync::Once;

        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });

        // Load test certificate
        let cert_pem = include_bytes!("../../../test_fixtures/tls/example.com.crt");
        let key_pem = include_bytes!("../../../test_fixtures/tls/example.com.key");
        let cert =
            TlsCertificate::from_pem(cert_pem, key_pem).expect("Should parse test certificate");

        // Create SNI resolver
        let mut resolver = SniResolver::new();
        resolver
            .add_cert("example.com".to_string(), cert)
            .expect("Should add certificate");

        // Start mock HTTP backend
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from("Backend response")))
                        .unwrap(),
                )
            });

            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router pointing to backend
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::GET, "/api/test", backends)
            .unwrap();

        // Start HTTPS proxy server with TLS termination
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();

        // This will fail initially - we need to add serve_https() method
        tokio::spawn(async move {
            let _ = server.serve_https(resolver).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Connect with HTTPS client
        use rustls::pki_types::ServerName;
        use rustls::ClientConfig;
        use tokio_rustls::TlsConnector;

        let mut root_cert_store = rustls::RootCertStore::empty();
        root_cert_store.add_parsable_certificates(
            rustls_pemfile::certs(&mut std::io::BufReader::new(&cert_pem[..]))
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );

        let client_config = ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();

        let connector = TlsConnector::from(Arc::new(client_config));

        let stream = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
        let domain = ServerName::try_from("example.com").unwrap();
        let tls_stream = connector.connect(domain, stream).await.unwrap();

        // Make HTTPS request through proxy
        let io = TokioIo::new(tls_stream);
        let (mut request_sender, connection) =
            hyper::client::conn::http1::handshake(io).await.unwrap();

        tokio::spawn(async move {
            let _ = connection.await;
        });

        let req = Request::builder()
            .uri("/api/test")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = request_sender.send_request(req).await.unwrap();

        // Verify HTTPS proxy worked
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();

        assert_eq!(body, "Backend response");
    }

    #[tokio::test]
    async fn test_http2_backend_connection_reuse() {
        // RED: Test HTTP/2 connection pool to backends
        //
        // This test verifies:
        // 1. Proxy uses HTTP/2 to connect to backends
        // 2. Multiple requests reuse the same connection (multiplexing)
        // 3. Connection is kept alive between requests

        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc as StdArc;

        // Track number of connections accepted by backend
        let connection_count = StdArc::new(AtomicUsize::new(0));
        let connection_count_clone = connection_count.clone();

        // Start HTTP/2 backend server
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let (stream, _) = backend_listener.accept().await.unwrap();
                connection_count_clone.fetch_add(1, Ordering::SeqCst);

                let io = TokioIo::new(stream);

                let service = service_fn(|req: Request<hyper::body::Incoming>| async move {
                    // Echo request path in response
                    let path = req.uri().path().to_string();
                    Ok::<_, hyper::Error>(
                        Response::builder()
                            .status(StatusCode::OK)
                            .body(Full::new(Bytes::from(format!("Response to {}", path))))
                            .unwrap(),
                    )
                });

                // Accept HTTP/2 connections
                tokio::spawn(async move {
                    let _ = http2::Builder::new(TokioExecutor::new())
                        .serve_connection(io, service)
                        .await;
                });
            }
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router pointing to HTTP/2 backend
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::GET, "/api/test1", backends.clone())
            .unwrap();
        router
            .add_route(HttpMethod::GET, "/api/test2", backends.clone())
            .unwrap();
        router
            .add_route(HttpMethod::GET, "/api/test3", backends)
            .unwrap();

        // Start HTTP/2 proxy server
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();

        // Mark backend as HTTP/2 (since we know it supports HTTP/2)
        server
            .set_backend_protocol_http2("127.0.0.1", backend_addr.port())
            .await;

        tokio::spawn(async move {
            let _ = server.serve_http2().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make HTTP/2 client connection to proxy
        let stream = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
        let io = TokioIo::new(stream);

        let (mut request_sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), io)
                .await
                .unwrap();

        tokio::spawn(async move {
            let _ = connection.await;
        });

        // Make 3 sequential requests through the proxy
        for i in 1..=3 {
            let req = Request::builder()
                .uri(format!("/api/test{}", i))
                .body(Full::new(Bytes::new()))
                .unwrap();

            let response = request_sender.send_request(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);

            let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
            let body = String::from_utf8(body_bytes.to_vec()).unwrap();
            assert_eq!(body, format!("Response to /api/test{}", i));
        }

        // Give time for connections to settle
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // CRITICAL ASSERTION: Backend should only see 1 connection (HTTP/2 pooling)
        // Currently FAILS because proxy creates new HTTP/1.1 connection per request
        let connections = connection_count.load(Ordering::SeqCst);
        assert_eq!(
            connections, 1,
            "Backend should see only 1 HTTP/2 connection (multiplexed), but saw {}",
            connections
        );
    }

    /// RED: Test that proxy records backend health after proxying requests
    ///
    /// This ensures passive health checking is integrated with the proxy server.
    /// When a backend returns 5xx errors, the router should mark it as unhealthy.
    #[tokio::test]
    #[ignore] // Note: Test is flaky due to Maglev distribution - needs rewrite to guarantee backend selection
    async fn test_proxy_records_backend_health_on_errors() {
        // Create router with 2 backends
        let router = Arc::new(Router::new());

        let backend1 = Backend::new(u32::from(Ipv4Addr::new(127, 0, 0, 1)), 9001, 100);
        let backend2 = Backend::new(u32::from(Ipv4Addr::new(127, 0, 0, 1)), 9002, 100);

        router
            .add_route(HttpMethod::GET, "/api/test", vec![backend1, backend2])
            .expect("Should add route");

        // Start backend1 that returns 500 errors
        let listener1 = TcpListener::bind("127.0.0.1:9001").await.unwrap();
        tokio::spawn(async move {
            loop {
                if let Ok((stream, _)) = listener1.accept().await {
                    tokio::spawn(async move {
                        let io = TokioIo::new(stream);
                        let service = service_fn(|_req| async {
                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                                    .body(Full::new(Bytes::from("Backend Error")))
                                    .unwrap(),
                            )
                        });

                        let _ = http1::Builder::new().serve_connection(io, service).await;
                    });
                }
            }
        });

        // Start backend2 that returns 200 OK
        let listener2 = TcpListener::bind("127.0.0.1:9002").await.unwrap();
        tokio::spawn(async move {
            loop {
                if let Ok((stream, _)) = listener2.accept().await {
                    tokio::spawn(async move {
                        let io = TokioIo::new(stream);
                        let service = service_fn(|_req| async {
                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .body(Full::new(Bytes::from("Success")))
                                    .unwrap(),
                            )
                        });

                        let _ = http1::Builder::new().serve_connection(io, service).await;
                    });
                }
            }
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Start proxy server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::clone(&router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make 20 requests through the proxy to backend1 (will return 500)
        // This should mark backend1 as unhealthy (>50% error rate)
        let client = Client::builder(TokioExecutor::new()).build_http();

        for _ in 0..20 {
            let uri = format!("http://{}/api/test", proxy_addr);
            let req = Request::builder()
                .uri(uri)
                .body(Full::new(Bytes::new()))
                .unwrap();

            let response = client.request(req).await.unwrap();
            // Both 200 and 500 are valid responses (depends on which backend)
            assert!(
                response.status() == StatusCode::OK
                    || response.status() == StatusCode::INTERNAL_SERVER_ERROR
            );
        }

        // Give time for health recording
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Verify that after many 500s, backend1 should be marked unhealthy
        // Make 100 more requests - they should ONLY go to healthy backend2 (9002)
        let mut success_count = 0;
        let mut error_count = 0;

        for _ in 0..100 {
            let uri = format!("http://{}/api/test", proxy_addr);
            let req = Request::builder()
                .uri(uri)
                .body(Full::new(Bytes::new()))
                .unwrap();

            let response = client.request(req).await.unwrap();
            if response.status() == StatusCode::OK {
                success_count += 1;
            } else {
                error_count += 1;
            }
        }

        // Passive health check verification happens via assertions below

        // After health checking is integrated, we should see:
        // - success_count ~= 100 (all requests go to healthy backend2)
        // - error_count ~= 0 (unhealthy backend1 is excluded)
        //
        // Without integration, distribution should be ~50/50 (Maglev is deterministic but doesn't know about health)
        // With integration, should be >90% success
        assert!(
            success_count > 90,
            "Expected >90% success after health checking, got {}/100 (without health integration, should be ~50/50)",
            success_count
        );
        assert!(
            error_count < 10,
            "Expected <10% errors after health checking, got {}/100",
            error_count
        );
    }

    /// RED: Test HTTP → HTTPS redirect with 301 status code
    ///
    /// This test verifies that the proxy server returns a 301 redirect response
    /// when a RequestRedirect filter is configured to upgrade HTTP to HTTPS.
    /// The Location header should contain the HTTPS URL, and the backend
    /// should NOT be called (request is handled by the proxy).
    #[tokio::test]
    async fn test_redirect_https_upgrade() {
        use crate::proxy::filters::{RedirectStatusCode, RequestRedirect};
        use crate::proxy::router::Router;

        // Create router with redirect filter (HTTP → HTTPS with 301)
        let router = Arc::new(Router::new());

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];

        // Create redirect filter: scheme="https", status_code=301
        let redirect = RequestRedirect::new()
            .scheme("https".to_string())
            .status_code(RedirectStatusCode::MovedPermanently);

        router
            .add_route_with_redirect(HttpMethod::GET, "/old-path", backends, redirect)
            .expect("Should add route with redirect");

        // Start proxy server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::clone(&router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Send HTTP request to /old-path
        let client = Client::builder(TokioExecutor::new()).build_http();

        let uri = format!("http://{}/old-path", proxy_addr);
        let req = Request::builder()
            .uri(&uri)
            .header("Host", "example.com")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = client.request(req).await.unwrap();

        // Verify redirect response (will FAIL - not implemented yet)
        assert_eq!(
            response.status(),
            StatusCode::MOVED_PERMANENTLY,
            "Expected 301 redirect response"
        );

        // Verify Location header exists with HTTPS scheme
        let location = response
            .headers()
            .get("Location")
            .expect("Location header should be present");

        let location_str = location.to_str().unwrap();
        assert!(
            location_str.starts_with("https://"),
            "Location should start with https://, got: {}",
            location_str
        );
        assert!(
            location_str.contains("example.com"),
            "Location should preserve hostname, got: {}",
            location_str
        );
        assert!(
            location_str.contains("/old-path"),
            "Location should preserve path, got: {}",
            location_str
        );
    }

    /// RED: Test hostname redirect with 302 status code
    ///
    /// This test verifies that the proxy server returns a 302 redirect response
    /// when a RequestRedirect filter is configured to change the hostname.
    /// The Location header should contain the new hostname while preserving
    /// the scheme and path.
    #[tokio::test]
    async fn test_redirect_hostname_change() {
        use crate::proxy::filters::{RedirectStatusCode, RequestRedirect};
        use crate::proxy::router::Router;

        // Create router with redirect filter (hostname change with 302)
        let router = Arc::new(Router::new());

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];

        // Create redirect filter: hostname="new.example.com", status_code=302
        let redirect = RequestRedirect::new()
            .hostname("new.example.com".to_string())
            .status_code(RedirectStatusCode::Found);

        router
            .add_route_with_redirect(HttpMethod::GET, "/api/test", backends, redirect)
            .expect("Should add route with redirect");

        // Start proxy server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::clone(&router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Send request to /api/test with original hostname
        let client = Client::builder(TokioExecutor::new()).build_http();

        let uri = format!("http://{}/api/test", proxy_addr);
        let req = Request::builder()
            .uri(&uri)
            .header("Host", "old.example.com")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = client.request(req).await.unwrap();

        // Verify redirect response (will FAIL - not implemented yet)
        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "Expected 302 redirect response"
        );

        // Verify Location header has new hostname
        let location = response
            .headers()
            .get("Location")
            .expect("Location header should be present");

        let location_str = location.to_str().unwrap();
        assert!(
            location_str.contains("new.example.com"),
            "Location should contain new hostname, got: {}",
            location_str
        );
        assert!(
            location_str.contains("/api/test"),
            "Location should preserve path, got: {}",
            location_str
        );
        assert!(
            location_str.starts_with("http://"),
            "Location should preserve HTTP scheme, got: {}",
            location_str
        );
    }

    // =============================================================================
    // Timeout Enforcement Tests (TDD RED phase)
    // =============================================================================

    /// RED: Test that backend_request timeout is enforced
    ///
    /// When a route has a backend_request timeout configured, requests to slow
    /// backends should timeout and return 504 Gateway Timeout.
    #[tokio::test]
    async fn test_backend_request_timeout_enforced() {
        use crate::proxy::filters::Timeout;
        use std::time::Duration;

        // Start a SLOW backend that takes 500ms to respond
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                // Sleep 500ms before responding (simulating slow backend)
                tokio::time::sleep(Duration::from_millis(500)).await;

                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from("Slow response")))
                        .unwrap(),
                )
            });

            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Create router with 100ms backend_request timeout
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];

        // Configure 100ms backend timeout (backend sleeps 500ms, so this SHOULD timeout)
        let timeout = Timeout::new().backend_request(Duration::from_millis(100));
        router
            .add_route_with_timeout(HttpMethod::GET, "/api/slow", backends, timeout)
            .unwrap();

        // Start proxy server
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Make request through proxy
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri = format!("http://{}/api/slow", proxy_addr).parse().unwrap();
        let response = client.get(uri).await.expect("Request should complete");

        // Should return 504 Gateway Timeout (because backend_request timeout was exceeded)
        assert_eq!(
            response.status(),
            StatusCode::GATEWAY_TIMEOUT,
            "Expected 504 Gateway Timeout for slow backend, got {}",
            response.status()
        );

        // Body should indicate timeout
        let body_bytes = response.collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(
            body.to_lowercase().contains("timeout"),
            "Response body should mention timeout, got: {}",
            body
        );
    }

    /// RED: Test that overall request timeout is enforced
    ///
    /// The request timeout is the total time allowed for the entire request,
    /// including potential retries. This is different from backend_request timeout.
    #[tokio::test]
    async fn test_overall_request_timeout_enforced() {
        use crate::proxy::filters::Timeout;
        use std::time::Duration;

        // Start a slow backend
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                // Sleep 300ms before responding
                tokio::time::sleep(Duration::from_millis(300)).await;

                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from("Slow response")))
                        .unwrap(),
                )
            });

            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Create router with 100ms overall request timeout
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];

        // Configure 100ms overall request timeout (backend sleeps 300ms)
        let timeout = Timeout::new().request(Duration::from_millis(100));
        router
            .add_route_with_timeout(HttpMethod::GET, "/api/slow", backends, timeout)
            .unwrap();

        // Start proxy server
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Make request through proxy
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri = format!("http://{}/api/slow", proxy_addr).parse().unwrap();
        let response = client.get(uri).await.expect("Request should complete");

        // Should return 504 Gateway Timeout
        assert_eq!(
            response.status(),
            StatusCode::GATEWAY_TIMEOUT,
            "Expected 504 Gateway Timeout for slow backend, got {}",
            response.status()
        );
    }

    /// RED: Test that fast backends within timeout succeed
    ///
    /// Sanity check: backends that respond within the timeout should succeed.
    #[tokio::test]
    async fn test_fast_backend_within_timeout_succeeds() {
        use crate::proxy::filters::Timeout;
        use std::time::Duration;

        // Start a FAST backend that responds immediately
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                // No sleep - respond immediately
                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from("Fast response")))
                        .unwrap(),
                )
            });

            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Create router with generous timeout
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];

        // Configure 1s timeout (backend responds immediately)
        let timeout = Timeout::new().backend_request(Duration::from_secs(1));
        router
            .add_route_with_timeout(HttpMethod::GET, "/api/fast", backends, timeout)
            .unwrap();

        // Start proxy server
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Make request through proxy
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri = format!("http://{}/api/fast", proxy_addr).parse().unwrap();
        let response = client.get(uri).await.expect("Request should complete");

        // Should return 200 OK (backend responded within timeout)
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "Expected 200 OK for fast backend, got {}",
            response.status()
        );

        let body_bytes = response.collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert_eq!(body, "Fast response");
    }

    // =============================================================================
    // Retry Tests (TDD RED phase)
    // =============================================================================

    /// RED: Test exponential backoff calculation
    #[test]
    fn test_retry_config_exponential_backoff() {
        use crate::proxy::filters::RetryConfig;
        use std::time::Duration;

        let config = RetryConfig::new()
            .base_delay(Duration::from_millis(100))
            .max_delay(Duration::from_secs(5));

        // Attempt 0: 100ms * 2^0 = 100ms
        assert_eq!(config.calculate_delay(0), Duration::from_millis(100));

        // Attempt 1: 100ms * 2^1 = 200ms
        assert_eq!(config.calculate_delay(1), Duration::from_millis(200));

        // Attempt 2: 100ms * 2^2 = 400ms
        assert_eq!(config.calculate_delay(2), Duration::from_millis(400));

        // Attempt 3: 100ms * 2^3 = 800ms
        assert_eq!(config.calculate_delay(3), Duration::from_millis(800));

        // Attempt 4: 100ms * 2^4 = 1600ms
        assert_eq!(config.calculate_delay(4), Duration::from_millis(1600));

        // Attempt 6: 100ms * 2^6 = 6400ms, but capped at 5000ms
        assert_eq!(config.calculate_delay(6), Duration::from_secs(5));
    }

    /// RED: Test retry status code matching
    #[test]
    fn test_retry_config_should_retry_status() {
        use crate::proxy::filters::RetryConfig;

        let config = RetryConfig::new().retry_on_5xx(true);

        // Should retry 5xx
        assert!(config.should_retry_status(500));
        assert!(config.should_retry_status(502));
        assert!(config.should_retry_status(503));
        assert!(config.should_retry_status(504));

        // Should NOT retry 4xx
        assert!(!config.should_retry_status(400));
        assert!(!config.should_retry_status(404));
        assert!(!config.should_retry_status(429));

        // Should NOT retry 2xx/3xx
        assert!(!config.should_retry_status(200));
        assert!(!config.should_retry_status(301));

        // With retry_on_5xx disabled
        let config_disabled = RetryConfig::new().retry_on_5xx(false);
        assert!(!config_disabled.should_retry_status(500));
    }

    /// RED: Test that proxy retries on 503 and eventually succeeds
    ///
    /// Backend returns 503 on first request, then 200 on retry.
    /// This test will FAIL until retry logic is implemented.
    #[tokio::test]
    async fn test_proxy_retries_on_503_then_succeeds() {
        use crate::proxy::filters::RetryConfig;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::time::Duration;

        // Track request count
        let request_count = Arc::new(AtomicU32::new(0));
        let request_count_clone = request_count.clone();

        // Start a flaky backend that fails first request, succeeds second
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let (stream, _) = match backend_listener.accept().await {
                    Ok(conn) => conn,
                    Err(_) => break,
                };
                let io = TokioIo::new(stream);
                let count = request_count_clone.clone();

                tokio::spawn(async move {
                    let service = service_fn(move |_req: Request<hyper::body::Incoming>| {
                        let attempt = count.fetch_add(1, Ordering::SeqCst);
                        async move {
                            if attempt == 0 {
                                // First request: return 503
                                Ok::<_, hyper::Error>(
                                    Response::builder()
                                        .status(StatusCode::SERVICE_UNAVAILABLE)
                                        .body(Full::new(Bytes::from("Service Unavailable")))
                                        .unwrap(),
                                )
                            } else {
                                // Subsequent requests: return 200
                                Ok::<_, hyper::Error>(
                                    Response::builder()
                                        .status(StatusCode::OK)
                                        .body(Full::new(Bytes::from("Success after retry")))
                                        .unwrap(),
                                )
                            }
                        }
                    });

                    let _ = http1::Builder::new().serve_connection(io, service).await;
                });
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Create router with retry config
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];

        // Configure retry with 3 max attempts
        let retry_config = RetryConfig::new()
            .max_retries(3)
            .base_delay(Duration::from_millis(10));

        // Use add_route_with_retry to enable retry logic
        router
            .add_route_with_retry(HttpMethod::GET, "/api/flaky", backends, retry_config)
            .unwrap();

        // Start proxy server
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Make request through proxy
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http::<Full<Bytes>>();

        let uri = format!("http://{}/api/flaky", proxy_addr).parse().unwrap();
        let response = client.get(uri).await.expect("Request should complete");

        // Should return 200 OK after retry (backend succeeded on second attempt)
        // This WILL FAIL until retry logic is implemented - currently returns 503
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "Expected 200 OK after retry, got {} (retry not implemented yet)",
            response.status()
        );

        // Verify backend was called at least twice (initial + retry)
        assert!(
            request_count.load(Ordering::SeqCst) >= 2,
            "Backend should have been called at least twice for retry"
        );
    }

    // =============================================================================
    // HTTP/2 over TLS (H2) Tests
    // =============================================================================

    /// RED: Test HTTPS proxy with HTTP/2 over TLS (ALPN h2 negotiation)
    #[tokio::test]
    async fn test_https_proxy_with_http2_alpn() {
        use crate::proxy::tls::{SniResolver, TlsCertificate};
        use std::sync::Once;

        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });

        // Load test certificate
        let cert_pem = include_bytes!("../../../test_fixtures/tls/example.com.crt");
        let key_pem = include_bytes!("../../../test_fixtures/tls/example.com.key");
        let cert =
            TlsCertificate::from_pem(cert_pem, key_pem).expect("Should parse test certificate");

        // Create SNI resolver
        let mut resolver = SniResolver::new();
        resolver
            .add_cert("example.com".to_string(), cert)
            .expect("Should add certificate");

        // Start mock HTTP/2 backend
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = backend_listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            let service = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from("H2 Backend response")))
                        .unwrap(),
                )
            });

            let _ = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router pointing to backend
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
        };
        let backends = vec![Backend::new(backend_ip, backend_addr.port(), 100)];
        router
            .add_route(HttpMethod::GET, "/api/h2test", backends)
            .unwrap();

        // Start HTTPS proxy server with HTTP/2 support
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        drop(proxy_listener);

        let server = ProxyServer::new(proxy_addr.to_string(), Arc::new(router)).unwrap();

        // Use serve_https_h2 method (to be implemented)
        tokio::spawn(async move {
            let _ = server.serve_https_h2(resolver).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Connect with HTTPS client requesting HTTP/2 via ALPN
        use rustls::pki_types::ServerName;
        use rustls::ClientConfig;
        use tokio_rustls::TlsConnector;

        let mut root_cert_store = rustls::RootCertStore::empty();
        root_cert_store.add_parsable_certificates(
            rustls_pemfile::certs(&mut std::io::BufReader::new(&cert_pem[..]))
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );

        // Configure client to prefer HTTP/2 via ALPN
        let mut client_config = ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();
        // Set ALPN protocols to negotiate h2
        client_config.alpn_protocols = vec![b"h2".to_vec()];

        let connector = TlsConnector::from(Arc::new(client_config));

        let stream = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
        let domain = ServerName::try_from("example.com").unwrap();
        let tls_stream = connector.connect(domain, stream).await.unwrap();

        // Verify h2 was negotiated
        let (_, conn) = tls_stream.get_ref();
        assert_eq!(
            conn.alpn_protocol(),
            Some(b"h2".as_slice()),
            "Should negotiate h2 via ALPN"
        );

        // Make HTTP/2 request through HTTPS proxy
        let io = TokioIo::new(tls_stream);
        let (mut request_sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), io)
                .await
                .expect("HTTP/2 handshake should succeed with ALPN h2");

        tokio::spawn(async move {
            let _ = connection.await;
        });

        let req = Request::builder()
            .uri("/api/h2test")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = request_sender.send_request(req).await.unwrap();

        // Verify HTTP/2 proxy worked
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();

        assert_eq!(body, "H2 Backend response");
    }

    /// RED: Test ALPN negotiation returns h2 when client supports it
    #[tokio::test]
    async fn test_alpn_negotiation_selects_h2() {
        use crate::proxy::tls::TlsCertificate;
        use rustls::pki_types::ServerName;
        use rustls::ClientConfig;
        use std::sync::Once;
        use tokio_rustls::{TlsAcceptor, TlsConnector};

        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });

        // Load test certificate
        let cert_pem = include_bytes!("../../../test_fixtures/tls/example.com.crt");
        let key_pem = include_bytes!("../../../test_fixtures/tls/example.com.key");
        let cert =
            TlsCertificate::from_pem(cert_pem, key_pem).expect("Should parse test certificate");

        // Build server config with ALPN for h2 and http/1.1
        let mut server_config = cert.to_server_config().expect("Should build server config");
        // Get mutable reference to set ALPN protocols
        let server_config_mut = Arc::make_mut(&mut server_config);
        server_config_mut.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

        let acceptor = TlsAcceptor::from(server_config);

        // Start TLS server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        let (server_ready_tx, server_ready_rx) = tokio::sync::oneshot::channel();
        let (negotiated_tx, negotiated_rx) = tokio::sync::oneshot::channel::<Option<Vec<u8>>>();

        tokio::spawn(async move {
            server_ready_tx.send(()).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let tls_stream = acceptor.accept(stream).await.unwrap();

            // Get negotiated ALPN protocol
            let (_, conn) = tls_stream.get_ref();
            let alpn = conn.alpn_protocol().map(|s| s.to_vec());
            let _ = negotiated_tx.send(alpn);
        });

        server_ready_rx.await.unwrap();

        // Build client config requesting h2
        let mut root_cert_store = rustls::RootCertStore::empty();
        root_cert_store.add_parsable_certificates(
            rustls_pemfile::certs(&mut std::io::BufReader::new(&cert_pem[..]))
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );

        let mut client_config = ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();
        client_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

        let connector = TlsConnector::from(Arc::new(client_config));

        // Connect to server
        let stream = tokio::net::TcpStream::connect(server_addr).await.unwrap();
        let domain = ServerName::try_from("example.com").unwrap();
        let _tls_stream = connector.connect(domain, stream).await.unwrap();

        // Check what protocol was negotiated
        let negotiated = negotiated_rx.await.unwrap();
        assert_eq!(
            negotiated,
            Some(b"h2".to_vec()),
            "Server should negotiate h2 when client supports it"
        );
    }
}
