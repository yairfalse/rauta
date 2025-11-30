//! Load Testing and Performance Validation
//!
//! Runs load tests using wrk and validates performance metrics.

use super::super::framework::{assertions, fixtures, k8s};
use super::super::framework::{TestContext, TestResult};
use super::super::{TestConfig, TestScenario};
use std::process::Command;
use std::time::Duration;

pub struct LoadTestScenario;

#[async_trait::async_trait]
impl TestScenario for LoadTestScenario {
    fn name(&self) -> &str {
        "load_testing"
    }

    async fn run(&self, ctx: &mut TestContext) -> TestResult {
        println!("\n🧪 Running Load Testing Scenarios");

        // Test 1: Baseline performance
        test_baseline_performance(ctx).await?;

        // Test 2: Sustained load
        test_sustained_load(ctx).await?;

        println!("✅ All load tests passed!\n");
        Ok(())
    }

    fn should_skip(&self, config: &TestConfig) -> bool {
        !config.scenarios.load_testing
    }
}

/// Test 1: Baseline performance (low load)
async fn test_baseline_performance(ctx: &TestContext) -> TestResult {
    println!("  📝 Test: Baseline Performance");

    // Deploy backend
    let backend_yaml = fixtures::test_backend("perf-backend", &ctx.namespace, 8080, 3);
    k8s::apply_yaml(&ctx.client, &backend_yaml).await?;

    // Wait for backends
    wait_for_ready_pods(ctx, "perf-backend", 3).await?;

    // Create Gateway + HTTPRoute
    let gateway_yaml = fixtures::gateway_with_http("perf-gateway", &ctx.namespace);
    k8s::apply_yaml(&ctx.client, &gateway_yaml).await?;

    assertions::wait_for_gateway_accepted(
        &ctx.client,
        &ctx.namespace,
        "perf-gateway",
        ctx.timeouts.gateway_ready,
    )
    .await?;

    let route_yaml = fixtures::http_route(
        "perf-route",
        &ctx.namespace,
        "perf-gateway",
        "/api",
        "perf-backend",
        8080,
    );
    k8s::apply_yaml(&ctx.client, &route_yaml).await?;

    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get RAUTA endpoint
    let endpoint = get_rauta_endpoint(ctx).await?;

    // Scrape baseline metrics
    println!("  📊 Scraping baseline metrics...");
    let metrics_url = format!("http://{}:9090/metrics", endpoint);
    ctx.metrics.scrape(&metrics_url).await?;
    ctx.metrics.set_baseline()?;

    // Run wrk load test (10s warmup)
    println!("  🔥 Running baseline load test (10s, 100 connections)...");
    run_wrk(&endpoint, "/api/test", 10, 100, 4)?;

    // Scrape final metrics
    ctx.metrics.scrape(&metrics_url).await?;

    // Analyze results
    if let Some(delta) = ctx.metrics.get_delta() {
        println!("  📈 Baseline results:");
        println!(
            "      Requests: {:?}",
            delta.requests_delta.values().sum::<u64>()
        );
        println!(
            "      p99 latency delta: {:.3}ms",
            delta.duration_p99_delta * 1000.0
        );
    }

    println!("  ✅ Baseline performance captured");

    Ok(())
}

/// Test 2: Sustained high load
async fn test_sustained_load(ctx: &TestContext) -> TestResult {
    println!("  📝 Test: Sustained Load (30s, 400 connections)");

    let endpoint = get_rauta_endpoint(ctx).await?;

    // Scrape baseline
    let metrics_url = format!("http://{}:9090/metrics", endpoint);
    ctx.metrics.scrape(&metrics_url).await?;
    ctx.metrics.set_baseline()?;

    // Run high load test
    println!("  🔥 Running sustained load test...");
    let output = run_wrk(&endpoint, "/api/load", 30, 400, 12)?;

    // Scrape final metrics
    ctx.metrics.scrape(&metrics_url).await?;

    // Parse wrk output
    let stats = parse_wrk_output(&output)?;

    println!("  📊 Load test results:");
    println!("      Requests/sec: {:.0}", stats.requests_per_sec);
    println!("      Avg latency: {:.2}ms", stats.avg_latency_ms);
    println!("      p99 latency: {:.2}ms", stats.p99_latency_ms);
    println!("      Total requests: {}", stats.total_requests);

    // Validate performance targets
    if stats.requests_per_sec < 10000.0 {
        return Err(format!(
            "Performance below target: {:.0} rps < 10,000 rps",
            stats.requests_per_sec
        )
        .into());
    }

    if stats.p99_latency_ms > 50.0 {
        return Err(format!(
            "p99 latency above target: {:.2}ms > 50ms",
            stats.p99_latency_ms
        )
        .into());
    }

    println!("  ✅ Sustained load test passed (targets met)");

    Ok(())
}

/// Check if wrk is installed
fn check_wrk_installed() -> Result<(), Box<dyn std::error::Error>> {
    match Command::new("wrk").arg("--version").output() {
        Ok(_) => Ok(()),
        Err(_) => Err(
            "wrk not installed. Install with: brew install wrk (macOS) or apt-get install wrk (Linux)"
                .into(),
        ),
    }
}

/// Helper: Run wrk load test
fn run_wrk(
    endpoint: &str,
    path: &str,
    duration_secs: u64,
    connections: usize,
    threads: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    // Check if wrk is installed before running
    check_wrk_installed()?;

    let url = format!("http://{}{}", endpoint, path);

    let output = Command::new("wrk")
        .args(&[
            "-t",
            &threads.to_string(),
            "-c",
            &connections.to_string(),
            "-d",
            &format!("{}s", duration_secs),
            "--latency",
            &url,
        ])
        .output()?;

    if !output.status.success() {
        return Err(format!("wrk failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Helper: Parse wrk output
fn parse_wrk_output(output: &str) -> Result<LoadTestStats, Box<dyn std::error::Error>> {
    let mut stats = LoadTestStats::default();

    for line in output.lines() {
        if line.contains("Requests/sec:") {
            if let Some(val) = line.split_whitespace().nth(1) {
                stats.requests_per_sec = val.parse().unwrap_or(0.0);
            }
        }

        if line.trim().starts_with("Latency") && line.contains("ms") {
            // Parse: "Latency     3.05ms    1.17ms  40.76ms   90.92%"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let avg_str = parts[1].trim_end_matches("ms");
                stats.avg_latency_ms = avg_str.parse().unwrap_or(0.0);
            }
        }

        if line.contains("99%") {
            // Parse: "99%      5.67ms"
            if let Some(val_str) = line.split_whitespace().nth(1) {
                let val = val_str.trim_end_matches("ms");
                stats.p99_latency_ms = val.parse().unwrap_or(0.0);
            }
        }

        if line.contains("requests in") {
            // Parse: "3,899,443 requests in 30.10s"
            if let Some(req_str) = line.split_whitespace().next() {
                let cleaned = req_str.replace(",", "");
                stats.total_requests = cleaned.parse().unwrap_or(0);
            }
        }
    }

    Ok(stats)
}

/// Helper: Get RAUTA endpoint (port-forward or LoadBalancer IP)
async fn get_rauta_endpoint(_ctx: &TestContext) -> Result<String, Box<dyn std::error::Error>> {
    // TODO: Implement actual endpoint discovery
    // For kind cluster, use port-forwarding
    // For cloud, use Gateway status.addresses

    Ok("127.0.0.1:8080".to_string())
}

/// Helper: Wait for pods to be ready
async fn wait_for_ready_pods(ctx: &TestContext, app_label: &str, count: usize) -> TestResult {
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::{Api, ListParams};

    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), &ctx.namespace);
    let start = std::time::Instant::now();

    while start.elapsed().as_secs() < ctx.timeouts.backend_ready {
        let lp = ListParams::default().labels(&format!("app={}", app_label));
        let pod_list = pods.list(&lp).await?;

        let ready = pod_list
            .items
            .iter()
            .filter(|pod| {
                pod.status
                    .as_ref()
                    .and_then(|s| s.conditions.as_ref())
                    .map(|c| {
                        c.iter()
                            .any(|cond| cond.type_ == "Ready" && cond.status == "True")
                    })
                    .unwrap_or(false)
            })
            .count();

        if ready >= count {
            println!("  ✅ {}/{} pods ready", ready, count);
            return Ok(());
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    Err(format!("Timeout waiting for {} pods", count).into())
}

#[derive(Debug, Default)]
struct LoadTestStats {
    requests_per_sec: f64,
    avg_latency_ms: f64,
    p99_latency_ms: f64,
    total_requests: u64,
}
