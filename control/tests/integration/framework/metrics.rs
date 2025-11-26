//! Prometheus metrics collection for performance testing

use std::collections::HashMap;
use std::sync::Mutex;

/// Metrics collector for scraping Prometheus metrics from RAUTA
pub struct MetricsCollector {
    baseline: Mutex<Option<Metrics>>,
    current: Mutex<Option<Metrics>>,
}

#[derive(Debug, Clone)]
pub struct Metrics {
    pub requests_total: HashMap<String, u64>,
    pub request_duration_p50: f64,
    pub request_duration_p95: f64,
    pub request_duration_p99: f64,
    pub backend_connections: u64,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            baseline: Mutex::new(None),
            current: Mutex::new(None),
        }
    }

    /// Scrape metrics from RAUTA /metrics endpoint
    pub async fn scrape(
        &self,
        endpoint: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = reqwest::get(endpoint).await?;
        let body = response.text().await?;

        let metrics = Self::parse_prometheus(&body)?;

        *self.current.lock().map_err(|e| format!("Lock poisoned: {}", e))? = Some(metrics);

        Ok(())
    }

    /// Set baseline metrics (before load test)
    pub fn set_baseline(&self) -> Result<(), String> {
        let current = self.current.lock().map_err(|e| format!("Lock poisoned: {}", e))?;
        *self.baseline.lock().map_err(|e| format!("Lock poisoned: {}", e))? = current.clone();
        Ok(())
    }

    /// Get delta since baseline
    pub fn get_delta(&self) -> Option<MetricsDelta> {
        let baseline = self.baseline.lock().ok()?;
        let current = self.current.lock().ok()?;

        match (baseline.as_ref(), current.as_ref()) {
            (Some(baseline), Some(current)) => Some(MetricsDelta {
                requests_delta: current
                    .requests_total
                    .iter()
                    .map(|(k, v)| {
                        let baseline_val = baseline.requests_total.get(k).unwrap_or(&0);
                        (k.clone(), v - baseline_val)
                    })
                    .collect(),
                duration_p99_delta: current.request_duration_p99 - baseline.request_duration_p99,
            }),
            _ => None,
        }
    }

    /// Parse Prometheus text format
    fn parse_prometheus(body: &str) -> Result<Metrics, Box<dyn std::error::Error>> {
        // Simple parser for Prometheus text format
        // In production, use prometheus-parse crate

        let mut requests_total = HashMap::new();
        let mut request_duration_p99 = 0.0;

        for line in body.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }

            if line.starts_with("rauta_requests_total") {
                // Parse: rauta_requests_total{method="GET",route="/api",status="200"} 12345
                if let Some(value_str) = line.split_whitespace().nth(1) {
                    if let Ok(value) = value_str.parse::<u64>() {
                        requests_total.insert(line.to_string(), value);
                    }
                }
            }

            if line.starts_with("rauta_request_duration_seconds") && line.contains("quantile=\"0.99\"")
            {
                if let Some(value_str) = line.split_whitespace().nth(1) {
                    if let Ok(value) = value_str.parse::<f64>() {
                        request_duration_p99 = value;
                    }
                }
            }
        }

        Ok(Metrics {
            requests_total,
            request_duration_p50: 0.0,
            request_duration_p95: 0.0,
            request_duration_p99,
            backend_connections: 0,
        })
    }
}

#[derive(Debug)]
pub struct MetricsDelta {
    pub requests_delta: HashMap<String, u64>,
    pub duration_p99_delta: f64,
}
