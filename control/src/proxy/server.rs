//! HTTP Server - proxies requests to backends

use crate::proxy::router::Router;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;
use lazy_static::lazy_static;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder,
};
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;
use tracing::{error, warn};

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

/// HTTP Proxy Server
pub struct ProxyServer {
    bind_addr: String,
    router: Arc<Router>,
    client: Client<HttpConnector, Full<Bytes>>,
}

impl ProxyServer {
    /// Create new proxy server
    pub fn new(bind_addr: String, router: Router) -> Result<Self, String> {
        // Create HTTP client with connection pooling
        let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();

        Ok(Self {
            bind_addr,
            router: Arc::new(router),
            client,
        })
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

            tokio::spawn(async move {
                let io = TokioIo::new(stream);

                let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let router = router.clone();
                    let client = client.clone();
                    async move { handle_request(req, router, client).await }
                });

                let _ = http1::Builder::new().serve_connection(io, service).await;
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
) -> Result<Response<Full<Bytes>>, String> {
    // Read the incoming request body
    let (parts, body) = req.into_parts();
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| {
            error!(
                error.message = %e,
                error.type = "request_body_read",
                "Failed to read request body"
            );
            format!("Failed to read request body: {}", e)
        })?
        .to_bytes();

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
                error.message = %e,
                error.type = "backend_request_build",
                backend_uri = %backend_uri,
                "Failed to build backend request"
            );
            format!("Failed to build backend request: {}", e)
        })?;

    // Send request to backend
    let backend_resp = client.request(backend_req).await.map_err(|e| {
        error!(
            error.message = %e,
            error.type = "backend_connection",
            network.peer.address = %ipv4_to_string(backend.ipv4),
            network.peer.port = backend.port,
            "Backend connection failed"
        );
        format!("Backend connection failed: {}", e)
    })?;

    // Stream response body from backend
    let (mut parts, body) = backend_resp.into_parts();
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| {
            error!(
                error.message = %e,
                error.type = "backend_response_read",
                network.peer.address = %ipv4_to_string(backend.ipv4),
                network.peer.port = backend.port,
                "Failed to read backend response"
            );
            format!("Failed to read backend response: {}", e)
        })?
        .to_bytes();

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
) -> Result<Response<Full<Bytes>>, String> {
    let path = req.uri().path().to_string();

    // Handle /metrics endpoint (early return, don't record metrics for this)
    if path == "/metrics" && *req.method() == hyper::Method::GET {
        // Force initialization of lazy_static metrics
        let _ = &*HTTP_REQUEST_DURATION;
        let _ = &*HTTP_REQUESTS_TOTAL;

        let mut buffer = vec![];
        let encoder = TextEncoder::new();
        let metric_families = METRICS_REGISTRY.gather();

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
            let result = forward_to_backend(req, route_match.backend, client).await;
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
                        error.message = %e,
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
                http.request.method = ?method,
                url.path = %path,
                duration_us = duration.as_micros() as u64,
                "No route found"
            );

            Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
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

        let server = ProxyServer::new("127.0.0.1:8080".to_string(), router);
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

        let server = ProxyServer::new(bind_addr.to_string(), router).unwrap();

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

        let server = ProxyServer::new(proxy_addr.to_string(), router).unwrap();
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

        let server = ProxyServer::new(proxy_addr.to_string(), router).unwrap();
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

        let server = ProxyServer::new(proxy_addr.to_string(), router).unwrap();
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

        let server = ProxyServer::new(proxy_addr.to_string(), router).unwrap();
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

        let server = ProxyServer::new(proxy_addr.to_string(), router).unwrap();
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

        let server = ProxyServer::new(proxy_addr.to_string(), router).unwrap();
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

        let server = ProxyServer::new(proxy_addr.to_string(), router).unwrap();
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

        let server = ProxyServer::new(proxy_addr.to_string(), router).unwrap();
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
}
