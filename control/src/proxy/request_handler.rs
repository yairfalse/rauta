//! HTTP Request Handler
//!
//! Handles incoming HTTP requests: routing, rate limiting, circuit breaking,
//! filter application, and response generation.

use crate::apis::metrics::CONTROLLER_METRICS_REGISTRY;
use crate::proxy::backend_pool::gather_pool_metrics;
use crate::proxy::circuit_breaker::CircuitBreakerManager;
use crate::proxy::filters::{
    HeaderModifierOp, RedirectStatusCode, RequestHeaderModifier, RequestRedirect,
    ResponseHeaderModifier, RetryConfig,
};
use crate::proxy::forwarder::{forward_to_backend, BackendPools, ProtocolCache, Workers};
use crate::proxy::metrics::{HTTP_REQUESTS_TOTAL, HTTP_REQUEST_DURATION, METRICS_REGISTRY};
use crate::proxy::rate_limiter::RateLimiter;
use crate::proxy::router::Router;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::Bytes;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use prometheus::{Encoder, TextEncoder};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Worker selector for round-robin distribution
pub struct WorkerSelector {
    counter: std::sync::atomic::AtomicUsize,
    num_workers: usize,
}

impl WorkerSelector {
    pub fn new(num_workers: usize) -> Self {
        Self {
            counter: std::sync::atomic::AtomicUsize::new(0),
            num_workers,
        }
    }

    /// Select next worker (round-robin)
    pub fn select(&self) -> usize {
        let current = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        current % self.num_workers
    }
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

/// Apply request header filters before forwarding to backend
/// Gateway API HTTPRouteBackendRequestHeaderModification
///
/// Security: HeaderName::from_bytes() and HeaderValue::from_str() validate input and reject
/// CRLF characters (\r\n), preventing header injection attacks. Invalid headers return errors.
pub fn apply_request_filters(
    req: &mut Request<hyper::body::Incoming>,
    filters: &RequestHeaderModifier,
) -> Result<(), String> {
    apply_header_filters(req, &filters.operations)
}

/// Apply response header filters after receiving response from backend
/// Gateway API HTTPResponseHeaderModifier
pub fn apply_response_filters<B>(
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
pub fn build_redirect_response(
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
                .map_err(|never| match never {})
                .boxed(),
        )
        .map_err(|e| format!("Failed to build redirect response: {}", e))
}

/// Convert HttpMethod to static string (zero allocations)
pub fn method_to_str(method: &common::HttpMethod) -> &'static str {
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
pub fn status_to_str(status: u16) -> &'static str {
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
pub async fn handle_request(
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
        return serve_metrics_endpoint().await;
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
                        error!(request_id = %request_id, error = %e, "Failed to build redirect response");
                        return build_error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Redirect error: {}", e),
                        );
                    }
                }
            }

            // Check rate limit for this route
            if !rate_limiter.check_rate_limit(&route_match.pattern) {
                warn!(request_id = %request_id, route.pattern = %route_match.pattern, "Rate limit exceeded");
                return build_rate_limit_response();
            }

            // Apply request header filters if configured
            if let Some(filters) = &route_match.request_filters {
                if let Err(e) = apply_request_filters(&mut req, filters) {
                    error!(request_id = %request_id, error = %e, "Failed to apply request header filters");
                    return build_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Filter error: {}", e),
                    );
                }
            }

            // Check circuit breaker for backend
            let backend_id = route_match.backend.to_string();
            if !circuit_breaker.allow_request(&backend_id) {
                warn!(request_id = %request_id, backend = %backend_id, "Circuit breaker is OPEN");
                return build_circuit_breaker_response();
            }

            info!(
                request_id = %request_id,
                stage = "route_matched",
                route.pattern = %route_match.pattern,
                network.peer.address = %route_match.backend,
                worker_index = ?worker_index,
                "Route matched, forwarding to backend"
            );

            // Execute request with optional retry logic
            let result = execute_request_with_retry(
                req,
                &route_match,
                method,
                &path,
                &request_id,
                client,
                backend_pools,
                protocol_cache,
                workers,
                worker_index,
            )
            .await;

            let duration = start.elapsed();

            // Record metrics and handle result
            record_request_metrics(
                &result,
                method,
                &route_match.pattern,
                worker_index,
                duration,
                &router,
                route_match.backend,
                &circuit_breaker,
                &backend_id,
                &request_id,
                &path,
            );

            // Apply response filters and return
            finalize_response(result, route_match.response_filters.as_ref(), &request_id)
        }
        None => {
            let duration = start.elapsed();
            record_not_found_metrics(method, duration, &request_id, &path);
            build_not_found_response(&request_id)
        }
    }
}

/// Serve the /metrics endpoint
async fn serve_metrics_endpoint() -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    // Force initialization of lazy_static metrics
    let _ = &*HTTP_REQUEST_DURATION;
    let _ = &*HTTP_REQUESTS_TOTAL;

    let mut buffer = vec![];
    let encoder = TextEncoder::new();

    // Gather metrics from all registries
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
        return build_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to encode metrics: {}", e),
        );
    }

    // Append HTTP/2 pool metrics
    if let Ok(pool_metrics_text) = gather_pool_metrics() {
        buffer.extend_from_slice(pool_metrics_text.as_bytes());
    } else {
        warn!("Failed to gather HTTP/2 pool metrics");
    }

    // Append GatewayIndex metrics
    let gateway_index_text = crate::apis::gateway::gateway_index::gateway_index_metrics();
    if !gateway_index_text.is_empty() {
        buffer.extend_from_slice(b"\n");
        buffer.extend_from_slice(gateway_index_text.as_bytes());
    }

    #[allow(clippy::unwrap_used)]
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", encoder.format_type())
        .body(
            Full::new(Bytes::from(buffer))
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap())
}

/// Execute request with optional retry logic
#[allow(clippy::too_many_arguments)]
async fn execute_request_with_retry(
    req: Request<hyper::body::Incoming>,
    route_match: &crate::proxy::router::RouteMatch,
    method: common::HttpMethod,
    path: &str,
    request_id: &str,
    client: Client<HttpConnector, Full<Bytes>>,
    backend_pools: BackendPools,
    protocol_cache: ProtocolCache,
    workers: Option<Workers>,
    worker_index: Option<usize>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    let overall_request_timeout = route_match.timeout.as_ref().and_then(|t| t.request);
    let retry_config = route_match.retry.as_ref();
    let is_retryable_method =
        method == common::HttpMethod::GET || method == common::HttpMethod::HEAD;

    if let (true, Some(retry_cfg)) = (is_retryable_method, retry_config) {
        execute_with_retry(
            req,
            route_match,
            path,
            request_id,
            client,
            backend_pools,
            protocol_cache,
            workers,
            worker_index,
            overall_request_timeout,
            retry_cfg,
            method,
        )
        .await
    } else {
        execute_without_retry(
            req,
            route_match,
            request_id,
            client,
            backend_pools,
            protocol_cache,
            workers,
            worker_index,
            overall_request_timeout,
        )
        .await
    }
}

/// Execute request with retry logic for GET/HEAD
#[allow(clippy::too_many_arguments)]
async fn execute_with_retry(
    req: Request<hyper::body::Incoming>,
    route_match: &crate::proxy::router::RouteMatch,
    path: &str,
    request_id: &str,
    client: Client<HttpConnector, Full<Bytes>>,
    backend_pools: BackendPools,
    protocol_cache: ProtocolCache,
    workers: Option<Workers>,
    worker_index: Option<usize>,
    overall_timeout: Option<std::time::Duration>,
    retry_cfg: &RetryConfig,
    method: common::HttpMethod,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    let max_attempts = retry_cfg.max_retries + 1;

    // First attempt
    let first_result = if let Some(timeout_duration) = overall_timeout {
        match tokio::time::timeout(
            timeout_duration,
            forward_to_backend(
                req,
                route_match.backend,
                client.clone(),
                request_id,
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
            request_id,
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
        return first_result;
    }

    // Retry loop
    let mut last_result = first_result;
    let mut attempt: u32 = 1;

    while attempt < max_attempts {
        let backoff_delay = retry_cfg.calculate_delay(attempt - 1);
        debug!(
            request_id = %request_id,
            attempt = attempt,
            backoff_ms = backoff_delay.as_millis(),
            "Retrying request after backoff"
        );
        tokio::time::sleep(backoff_delay).await;

        // Rebuild request for retry
        let backend_uri = format!(
            "http://{}/{}",
            route_match.backend,
            path.trim_start_matches('/')
        );

        let retry_method = match method {
            common::HttpMethod::GET => hyper::Method::GET,
            common::HttpMethod::HEAD => hyper::Method::HEAD,
            _ => hyper::Method::GET,
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

        let retry_result = match client.request(retry_req).await {
            Ok(resp) => {
                let (parts, body) = resp.into_parts();
                Ok(Response::from_parts(parts, body.map_err(|e| e).boxed()))
            }
            Err(e) => Err(format!("Retry request failed: {}", e)),
        };

        let should_continue = match &retry_result {
            Ok(resp) => retry_cfg.should_retry_status(resp.status().as_u16()),
            Err(_) => retry_cfg.retry_on_connection_error,
        };

        last_result = retry_result;
        attempt += 1;

        if !should_continue {
            break;
        }
    }

    last_result
}

/// Execute request without retry
#[allow(clippy::too_many_arguments)]
async fn execute_without_retry(
    req: Request<hyper::body::Incoming>,
    route_match: &crate::proxy::router::RouteMatch,
    request_id: &str,
    client: Client<HttpConnector, Full<Bytes>>,
    backend_pools: BackendPools,
    protocol_cache: ProtocolCache,
    workers: Option<Workers>,
    worker_index: Option<usize>,
    overall_timeout: Option<std::time::Duration>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    if let Some(timeout_duration) = overall_timeout {
        match tokio::time::timeout(
            timeout_duration,
            forward_to_backend(
                req,
                route_match.backend,
                client,
                request_id,
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
            request_id,
            backend_pools,
            protocol_cache,
            workers,
            worker_index,
            route_match.timeout.as_ref(),
        )
        .await
    }
}

/// Record request metrics
#[allow(clippy::too_many_arguments)]
fn record_request_metrics(
    result: &Result<Response<BoxBody<Bytes, hyper::Error>>, String>,
    method: common::HttpMethod,
    route_pattern: &str,
    worker_index: Option<usize>,
    duration: std::time::Duration,
    router: &Router,
    backend: common::Backend,
    circuit_breaker: &CircuitBreakerManager,
    backend_id: &str,
    request_id: &str,
    path: &str,
) {
    let method_str = method_to_str(&method);
    let worker_label = worker_index
        .map(|i| i.to_string())
        .unwrap_or_else(|| "none".to_string());

    match result {
        Ok(resp) => {
            let status_code = resp.status().as_u16();
            let status_str = status_to_str(status_code);
            HTTP_REQUESTS_TOTAL
                .with_label_values(&[method_str, route_pattern, status_str, &worker_label])
                .inc();
            HTTP_REQUEST_DURATION
                .with_label_values(&[method_str, route_pattern, status_str])
                .observe(duration.as_secs_f64());

            router.record_backend_response(backend, status_code);

            if status_code >= 500 {
                circuit_breaker.record_failure(backend_id);
            } else {
                circuit_breaker.record_success(backend_id);
            }
        }
        Err(e) => {
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

            router.record_backend_response(backend, status_code);
            circuit_breaker.record_failure(backend_id);

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
}

/// Record 404 not found metrics
fn record_not_found_metrics(
    method: common::HttpMethod,
    duration: std::time::Duration,
    request_id: &str,
    path: &str,
) {
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
}

/// Finalize response with optional response filters
fn finalize_response(
    result: Result<Response<BoxBody<Bytes, hyper::Error>>, String>,
    response_filters: Option<&ResponseHeaderModifier>,
    request_id: &str,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    if let Some(filters) = response_filters {
        match result {
            Ok(mut resp) => {
                if let Err(e) = apply_response_filters(&mut resp, filters) {
                    error!(request_id = %request_id, error = %e, "Failed to apply response header filters");
                    build_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Response filter error: {}", e),
                    )
                } else {
                    Ok(resp)
                }
            }
            Err(e) => error_to_response(e, request_id),
        }
    } else {
        match result {
            Ok(resp) => Ok(resp),
            Err(e) => error_to_response(e, request_id),
        }
    }
}

/// Convert error string to HTTP response
fn error_to_response(
    error: String,
    request_id: &str,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    let status = if error.starts_with("TIMEOUT:") {
        StatusCode::GATEWAY_TIMEOUT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };

    #[allow(clippy::unwrap_used)]
    Ok(Response::builder()
        .status(status)
        .header("Content-Type", "text/plain")
        .header("X-Request-ID", request_id)
        .body(
            Full::new(Bytes::from(error))
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap())
}

/// Build generic error response
fn build_error_response(
    status: StatusCode,
    message: String,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    #[allow(clippy::unwrap_used)]
    Ok(Response::builder()
        .status(status)
        .header("Content-Type", "text/plain")
        .body(
            Full::new(Bytes::from(message))
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap())
}

/// Build 404 not found response
fn build_not_found_response(
    request_id: &str,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    #[allow(clippy::unwrap_used)]
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

/// Build rate limit exceeded response
fn build_rate_limit_response() -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    #[allow(clippy::unwrap_used)]
    Ok(Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("Content-Type", "text/plain")
        .header("Retry-After", "1")
        .body(
            Full::new(Bytes::from("Rate limit exceeded"))
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap())
}

/// Build circuit breaker open response
fn build_circuit_breaker_response() -> Result<Response<BoxBody<Bytes, hyper::Error>>, String> {
    #[allow(clippy::unwrap_used)]
    Ok(Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "text/plain")
        .header("Retry-After", "30")
        .body(
            Full::new(Bytes::from(
                "Backend temporarily unavailable (circuit breaker open)",
            ))
            .map_err(|never| match never {})
            .boxed(),
        )
        .unwrap())
}
