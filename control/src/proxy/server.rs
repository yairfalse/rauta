//! HTTP Server - proxies requests to backends

use crate::apis::metrics::CONTROLLER_METRICS_REGISTRY;
use crate::proxy::router::Router;
use http_body_util::{BodyExt, Full};
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
    static ref HTTP_REQUEST_DURATION: HistogramVec = {
        let opts = HistogramOpts::new(
            "http_request_duration_seconds",
            "HTTP request latencies in seconds",
        )
        .buckets(vec![
            0.001, 0.005, 0.010, 0.025, 0.050, 0.075, 0.100, 0.250, 0.500, 1.000, 2.500, 5.000,
        ]);
        let histogram = HistogramVec::new(opts, &["method", "path", "status"])
            .expect("Failed to create HTTP request duration histogram");
        METRICS_REGISTRY
            .register(Box::new(histogram.clone()))
            .expect("Failed to register HTTP request duration histogram with metrics registry");
        histogram
    };

    /// HTTP request counter
    static ref HTTP_REQUESTS_TOTAL: IntCounterVec = {
        let opts = Opts::new("http_requests_total", "Total number of HTTP requests");
        let counter = IntCounterVec::new(opts, &["method", "path", "status"])
            .expect("Failed to create HTTP request counter");
        METRICS_REGISTRY
            .register(Box::new(counter.clone()))
            .expect("Failed to register HTTP request counter with metrics registry");
        counter
    };
}

use crate::proxy::backend_pool::{BackendConnectionPools, PoolError};

/// Production-grade HTTP/2 backend connection pools
/// NOTE: Arc<Mutex> is temporary until Stage 2 per-core workers
/// In Stage 2, each worker will own their BackendConnectionPools (no lock!)
type BackendPools = Arc<Mutex<BackendConnectionPools>>;

/// Protocol detection cache - tracks which backends support HTTP/2
type ProtocolCache = Arc<Mutex<HashMap<String, bool>>>; // true = HTTP/2, false = HTTP/1.1

/// HTTP Proxy Server
pub struct ProxyServer {
    bind_addr: String,
    router: Arc<Router>,
    client: Client<HttpConnector, Full<Bytes>>,
    backend_pools: BackendPools,
    protocol_cache: ProtocolCache,
}

impl ProxyServer {
    /// Create new proxy server
    pub fn new(bind_addr: String, router: Arc<Router>) -> Result<Self, String> {
        // Create HTTP client with connection pooling
        let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();

        // Create production-grade HTTP/2 backend connection pools
        let backend_pools = Arc::new(Mutex::new(BackendConnectionPools::new()));

        // Create protocol detection cache
        let protocol_cache = Arc::new(Mutex::new(HashMap::new()));

        Ok(Self {
            bind_addr,
            router,
            client,
            backend_pools,
            protocol_cache,
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

            tokio::spawn(async move {
                let io = TokioIo::new(stream);

                let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let router = router.clone();
                    let client = client.clone();
                    let backend_pools = backend_pools.clone();
                    let protocol_cache = protocol_cache.clone();
                    async move {
                        handle_request(req, router, client, backend_pools, protocol_cache).await
                    }
                });

                let _ = http1::Builder::new().serve_connection(io, service).await;
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

            tokio::spawn(async move {
                let io = TokioIo::new(stream);

                let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let router = router.clone();
                    let client = client.clone();
                    let backend_pools = backend_pools.clone();
                    let protocol_cache = protocol_cache.clone();
                    async move {
                        handle_request(req, router, client, backend_pools, protocol_cache).await
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

/// Forward request to backend server
async fn forward_to_backend(
    req: Request<hyper::body::Incoming>,
    backend: common::Backend,
    client: Client<HttpConnector, Full<Bytes>>,
    request_id: &str,
    backend_pools: BackendPools,
    protocol_cache: ProtocolCache,
) -> Result<Response<Full<Bytes>>, String> {
    let request_start = Instant::now();

    // Read the incoming request body
    let (parts, body) = req.into_parts();
    let body_read_start = Instant::now();
    let body_bytes = body
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
        body_size_bytes = body_bytes.len(),
        elapsed_us = body_read_duration.as_micros() as u64,
        "Request body read complete"
    );

    // Build backend URI with path and query parameters
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let backend_uri = format!(
        "http://{}:{}{}",
        ipv4_to_string(backend.ipv4),
        backend.port,
        path_and_query
    );

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

    // Set Host header to match backend address
    let backend_host = format!("{}:{}", ipv4_to_string(backend.ipv4), backend.port);
    backend_req_builder = backend_req_builder.header("Host", backend_host);

    let backend_req = backend_req_builder
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

    info!(
        request_id = %request_id,
        stage = "backend_request_built",
        network.peer.address = %ipv4_to_string(backend.ipv4),
        network.peer.port = backend.port,
        url.full = %backend_uri,
        http.request.method = %method_str,
        "Sending request to backend"
    );

    // Try HTTP/2 first (with production connection pool), fallback to HTTP/1.1
    let backend_key = format!("{}:{}", ipv4_to_string(backend.ipv4), backend.port);
    let use_http2 = {
        let cache = protocol_cache.lock().await;
        cache.get(&backend_key).copied()
    };

    let backend_connect_start = Instant::now();
    let backend_resp = match use_http2 {
        Some(true) => {
            // Backend is known to support HTTP/2, use production connection pool
            debug!(request_id = %request_id, "Using HTTP/2 connection pool for backend {}", backend_key);

            let mut sender = {
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

            sender.send_request(backend_req).await.map_err(|e| {
                error!(
                    request_id = %request_id,
                    error.message = %e,
                    error.type = "http2_request",
                    "HTTP/2 request failed"
                );
                format!("HTTP/2 request failed: {}", e)
            })?
        }
        Some(false) | None => {
            // Backend uses HTTP/1.1 (cached or unknown)
            debug!(request_id = %request_id, "Using HTTP/1.1 for backend {}", backend_key);

            // Cache as HTTP/1.1 if unknown
            if use_http2.is_none() {
                let mut cache = protocol_cache.lock().await;
                cache.insert(backend_key.clone(), false);
            }

            client.request(backend_req).await.map_err(|e| {
                error!(
                    request_id = %request_id,
                    error.message = %e,
                    error.type = "backend_connection",
                    network.peer.address = %ipv4_to_string(backend.ipv4),
                    network.peer.port = backend.port,
                    elapsed_us = backend_connect_start.elapsed().as_micros() as u64,
                    "Backend connection failed"
                );
                format!("Backend connection failed: {}", e)
            })?
        }
    };

    let backend_connect_duration = backend_connect_start.elapsed();
    let response_status = backend_resp.status().as_u16();

    info!(
        request_id = %request_id,
        stage = "backend_response_received",
        http.response.status_code = response_status,
        network.peer.address = %ipv4_to_string(backend.ipv4),
        network.peer.port = backend.port,
        elapsed_us = backend_connect_duration.as_micros() as u64,
        "Backend responded"
    );

    // Stream response body from backend
    let (mut parts, body) = backend_resp.into_parts();
    let response_read_start = Instant::now();
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| {
            error!(
                request_id = %request_id,
                error.message = %e,
                error.type = "backend_response_read",
                network.peer.address = %ipv4_to_string(backend.ipv4),
                network.peer.port = backend.port,
                elapsed_us = response_read_start.elapsed().as_micros() as u64,
                "Failed to read backend response"
            );
            format!("Failed to read backend response: {}", e)
        })?
        .to_bytes();

    let response_read_duration = response_read_start.elapsed();
    info!(
        request_id = %request_id,
        stage = "backend_response_body_read",
        body_size_bytes = body_bytes.len(),
        elapsed_us = response_read_duration.as_micros() as u64,
        "Response body read complete"
    );

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
        network.peer.address = %ipv4_to_string(backend.ipv4),
        network.peer.port = backend.port,
        timing.total_us = total_duration.as_micros() as u64,
        timing.body_read_us = body_read_duration.as_micros() as u64,
        timing.backend_us = backend_connect_duration.as_micros() as u64,
        timing.response_read_us = response_read_duration.as_micros() as u64,
        "Request forwarded successfully"
    );

    Ok(Response::from_parts(parts, Full::new(body_bytes)))
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
async fn handle_request(
    req: Request<hyper::body::Incoming>,
    router: Arc<Router>,
    client: Client<HttpConnector, Full<Bytes>>,
    backend_pools: BackendPools,
    protocol_cache: ProtocolCache,
) -> Result<Response<Full<Bytes>>, String> {
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

        // Gather metrics from both registries (proxy + controller)
        let mut metric_families = METRICS_REGISTRY.gather();
        let controller_metrics = CONTROLLER_METRICS_REGISTRY.gather();
        metric_families.extend(controller_metrics);

        // Handle encoding failure gracefully
        if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
            error!("Failed to encode metrics: {}", e);
            return Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("Content-Type", "text/plain")
                .body(Full::new(Bytes::from(format!(
                    "Failed to encode metrics: {}",
                    e
                ))))
                .unwrap());
        }

        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", encoder.format_type())
            .body(Full::new(Bytes::from(buffer)))
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
            info!(
                request_id = %request_id,
                stage = "route_matched",
                route.pattern = %route_match.pattern,
                network.peer.address = %ipv4_to_string(route_match.backend.ipv4),
                network.peer.port = route_match.backend.port,
                "Route matched, forwarding to backend"
            );

            let result = forward_to_backend(
                req,
                route_match.backend,
                client,
                &request_id,
                backend_pools,
                protocol_cache,
            )
            .await;
            let duration = start.elapsed();

            // Record metrics (zero-allocation with static strings)
            // Use route pattern (not actual path) to prevent cardinality explosion
            let method_str = method_to_str(&method);
            let route_pattern = &route_match.pattern;
            match &result {
                Ok(resp) => {
                    let status_str = status_to_str(resp.status().as_u16());
                    HTTP_REQUESTS_TOTAL
                        .with_label_values(&[method_str, route_pattern, status_str])
                        .inc();
                    HTTP_REQUEST_DURATION
                        .with_label_values(&[method_str, route_pattern, status_str])
                        .observe(duration.as_secs_f64());
                }
                Err(e) => {
                    HTTP_REQUESTS_TOTAL
                        .with_label_values(&[method_str, route_pattern, "500"])
                        .inc();
                    HTTP_REQUEST_DURATION
                        .with_label_values(&[method_str, route_pattern, "500"])
                        .observe(duration.as_secs_f64());

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

            result
        }
        None => {
            let duration = start.elapsed();

            // Record 404 metrics (use constant to prevent cardinality explosion)
            let method_str = method_to_str(&method);
            HTTP_REQUESTS_TOTAL
                .with_label_values(&[method_str, "not_found", "404"])
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

            Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header("X-Request-ID", request_id)
                .body(Full::new(Bytes::from("Not Found")))
                .unwrap())
        }
    }
}

#[cfg(test)]
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

        // Backend responds with "Hello from backend"
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

            let _ = http1::Builder::new().serve_connection(io, service).await;
        });

        // Wait for backend to start
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Create router pointing to our mock backend
        let router = crate::proxy::router::Router::new();
        let backend_ip = match backend_addr.ip() {
            std::net::IpAddr::V4(ipv4) => u32::from(ipv4),
            _ => panic!("Expected IPv4 address"),
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

            let _ = http1::Builder::new().serve_connection(io, service).await;
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

            let _ = http1::Builder::new().serve_connection(io, service).await;
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

            let _ = http1::Builder::new().serve_connection(io, service).await;
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

            let _ = http1::Builder::new().serve_connection(io, service).await;
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

            let _ = http1::Builder::new().serve_connection(io, service).await;
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

            let _ = http1::Builder::new().serve_connection(io, service).await;
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

            let _ = http1::Builder::new().serve_connection(io, service).await;
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

            let _ = http1::Builder::new().serve_connection(io, service).await;
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
}
