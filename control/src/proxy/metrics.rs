//! HTTP Proxy Metrics
//!
//! Prometheus metrics for the HTTP proxy layer.
//! Extracted from server.rs for better modularity.

use lazy_static::lazy_static;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder,
};

lazy_static! {
    /// Global metrics registry for proxy metrics
    pub static ref METRICS_REGISTRY: Registry = Registry::new();

    /// HTTP request duration histogram (in seconds)
    ///
    /// Note: Fallback metrics use .expect() as last line of defense - if Prometheus itself is broken, we should panic
    #[allow(clippy::expect_used)]
    pub static ref HTTP_REQUEST_DURATION: HistogramVec = {
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
    pub static ref HTTP_REQUESTS_TOTAL: IntCounterVec = {
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

/// Encode all proxy metrics to Prometheus text format
#[allow(dead_code)]
pub fn encode_metrics() -> Result<Vec<u8>, String> {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();

    let metric_families = METRICS_REGISTRY.gather();
    encoder
        .encode(&metric_families, &mut buffer)
        .map_err(|e| format!("Failed to encode metrics: {}", e))?;

    Ok(buffer)
}

/// Get the Prometheus text encoder format type
#[allow(dead_code)]
pub fn metrics_content_type() -> &'static str {
    "text/plain; version=0.0.4; charset=utf-8"
}
