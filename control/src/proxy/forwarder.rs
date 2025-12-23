//! Backend Request Forwarding
//!
//! Handles forwarding HTTP requests to backend servers.
//! Includes protocol detection (HTTP/1.1 vs HTTP/2), connection pooling,
//! timeout enforcement, and hop-by-hop header filtering.

use crate::proxy::backend_pool::{BackendConnectionPools, PoolError};
use crate::proxy::filters::Timeout;
use crate::proxy::worker::Worker;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Request, Response};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Production-grade HTTP/2 backend connection pools
pub type BackendPools = Arc<Mutex<BackendConnectionPools>>;

/// Per-core workers (lock-free architecture)
pub type Workers = Arc<Vec<Worker>>;

/// Protocol detection cache - tracks which backends support HTTP/2
pub type ProtocolCache = Arc<Mutex<HashMap<String, bool>>>; // true = HTTP/2, false = HTTP/1.1

/// Check if a header is hop-by-hop and should not be forwarded
/// Per RFC 2616 Section 13.5.1
pub fn is_hop_by_hop_header(name: &str) -> bool {
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

/// Convert IPv4 u32 to string format (e.g., "192.168.1.1")
#[allow(dead_code)]
pub fn ipv4_to_string(ipv4: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        (ipv4 >> 24) & 0xFF,
        (ipv4 >> 16) & 0xFF,
        (ipv4 >> 8) & 0xFF,
        ipv4 & 0xFF
    )
}

/// Forward request to backend server
/// Returns BoxBody to support zero-copy streaming for HTTP/2 (hot path)
///
/// # Timeout Enforcement
/// If `timeout_config` is provided with a `backend_request` timeout, the HTTP request
/// to the backend will be wrapped in `tokio::time::timeout`. If the backend doesn't
/// respond within the timeout, returns an error that the caller should map to 504.
#[allow(clippy::too_many_arguments)]
pub async fn forward_to_backend(
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
                                .header("Host", backend.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_hop_by_hop_header() {
        assert!(is_hop_by_hop_header("connection"));
        assert!(is_hop_by_hop_header("Connection"));
        assert!(is_hop_by_hop_header("keep-alive"));
        assert!(is_hop_by_hop_header("transfer-encoding"));
        assert!(!is_hop_by_hop_header("content-type"));
        assert!(!is_hop_by_hop_header("x-custom-header"));
    }

    #[test]
    fn test_ipv4_to_string() {
        assert_eq!(ipv4_to_string(0x7F000001), "127.0.0.1");
        assert_eq!(ipv4_to_string(0xC0A80001), "192.168.0.1");
        assert_eq!(ipv4_to_string(0x0A000101), "10.0.1.1");
    }
}
