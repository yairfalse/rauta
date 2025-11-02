//! Controller metrics
//!
//! Minimal implementation to make tests pass

use lazy_static::lazy_static;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder,
};

lazy_static! {
    /// Controller metrics registry
    pub static ref CONTROLLER_METRICS_REGISTRY: Registry = Registry::new();

    /// HTTPRoute reconciliation duration
    static ref HTTPROUTE_RECONCILIATION_DURATION: HistogramVec = {
        let opts = HistogramOpts::new(
            "httproute_reconciliation_duration_seconds",
            "HTTPRoute reconciliation duration in seconds",
        );
        let histogram = HistogramVec::new(opts, &["httproute", "namespace"])
            .expect("Failed to create histogram");
        CONTROLLER_METRICS_REGISTRY
            .register(Box::new(histogram.clone()))
            .expect("Failed to register histogram");
        histogram
    };

    /// HTTPRoute reconciliations total
    static ref HTTPROUTE_RECONCILIATIONS_TOTAL: IntCounterVec = {
        let opts = Opts::new(
            "httproute_reconciliations_total",
            "Total number of httproute reconciliations",
        );
        let counter = IntCounterVec::new(opts, &["httproute", "namespace", "result"])
            .expect("Failed to create counter");
        CONTROLLER_METRICS_REGISTRY
            .register(Box::new(counter.clone()))
            .expect("Failed to register counter");
        counter
    };

    /// Gateway reconciliation duration
    static ref GATEWAY_RECONCILIATION_DURATION: HistogramVec = {
        let opts = HistogramOpts::new(
            "gateway_reconciliation_duration_seconds",
            "Gateway reconciliation duration in seconds",
        );
        let histogram = HistogramVec::new(opts, &["gateway", "namespace"])
            .expect("Failed to create histogram");
        CONTROLLER_METRICS_REGISTRY
            .register(Box::new(histogram.clone()))
            .expect("Failed to register histogram");
        histogram
    };

    /// Gateway reconciliations total
    static ref GATEWAY_RECONCILIATIONS_TOTAL: IntCounterVec = {
        let opts = Opts::new(
            "gateway_reconciliations_total",
            "Total number of gateway reconciliations",
        );
        let counter = IntCounterVec::new(opts, &["gateway", "namespace", "result"])
            .expect("Failed to create counter");
        CONTROLLER_METRICS_REGISTRY
            .register(Box::new(counter.clone()))
            .expect("Failed to register counter");
        counter
    };

    /// GatewayClass reconciliation duration
    static ref GATEWAYCLASS_RECONCILIATION_DURATION: HistogramVec = {
        let opts = HistogramOpts::new(
            "gatewayclass_reconciliation_duration_seconds",
            "GatewayClass reconciliation duration in seconds",
        );
        let histogram = HistogramVec::new(opts, &["gatewayclass"])
            .expect("Failed to create histogram");
        CONTROLLER_METRICS_REGISTRY
            .register(Box::new(histogram.clone()))
            .expect("Failed to register histogram");
        histogram
    };

    /// GatewayClass reconciliations total
    static ref GATEWAYCLASS_RECONCILIATIONS_TOTAL: IntCounterVec = {
        let opts = Opts::new(
            "gatewayclass_reconciliations_total",
            "Total number of gatewayclass reconciliations",
        );
        let counter = IntCounterVec::new(opts, &["gatewayclass", "result"])
            .expect("Failed to create counter");
        CONTROLLER_METRICS_REGISTRY
            .register(Box::new(counter.clone()))
            .expect("Failed to register counter");
        counter
    };
}

/// Record HTTPRoute reconciliation
#[allow(dead_code)] // Used in K8s mode
pub fn record_httproute_reconciliation(
    httproute: &str,
    namespace: &str,
    duration_secs: f64,
    result: &str,
) {
    HTTPROUTE_RECONCILIATION_DURATION
        .with_label_values(&[httproute, namespace])
        .observe(duration_secs);

    HTTPROUTE_RECONCILIATIONS_TOTAL
        .with_label_values(&[httproute, namespace, result])
        .inc();
}

/// Record Gateway reconciliation
#[allow(dead_code)] // Used in tests, will be used in Gateway reconcile
pub fn record_gateway_reconciliation(
    gateway: &str,
    namespace: &str,
    duration_secs: f64,
    result: &str,
) {
    GATEWAY_RECONCILIATION_DURATION
        .with_label_values(&[gateway, namespace])
        .observe(duration_secs);

    GATEWAY_RECONCILIATIONS_TOTAL
        .with_label_values(&[gateway, namespace, result])
        .inc();
}

/// Record GatewayClass reconciliation
#[allow(dead_code)] // Used in tests, will be used in GatewayClass reconcile
pub fn record_gatewayclass_reconciliation(gatewayclass: &str, duration_secs: f64, result: &str) {
    GATEWAYCLASS_RECONCILIATION_DURATION
        .with_label_values(&[gatewayclass])
        .observe(duration_secs);

    GATEWAYCLASS_RECONCILIATIONS_TOTAL
        .with_label_values(&[gatewayclass, result])
        .inc();
}

/// Gather controller metrics
#[allow(dead_code)] // Used in tests and will be used for metrics endpoint
pub fn gather_controller_metrics() -> Result<String, String> {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    let metric_families = CONTROLLER_METRICS_REGISTRY.gather();
    encoder
        .encode(&metric_families, &mut buffer)
        .map_err(|e| format!("Failed to encode metrics: {}", e))?;

    String::from_utf8(buffer).map_err(|e| format!("Failed to convert to UTF-8: {}", e))
}
