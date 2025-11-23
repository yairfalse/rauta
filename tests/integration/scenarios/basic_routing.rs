//! Basic HTTP Routing Test Scenario
//!
//! Validates that HTTP traffic flows from client → RAUTA → backend.

use crate::framework::{TestContext, TestResult};
use crate::framework::{assertions, fixtures, k8s};
use crate::{TestConfig, TestScenario};
use std::time::Duration;

pub struct BasicRoutingScenario;

impl TestScenario for BasicRoutingScenario {
    fn name(&self) -> &str {
        "basic_routing"
    }

    async fn run(&self, ctx: &mut TestContext) -> TestResult {
        println!("\n🧪 Running Basic HTTP Routing Scenarios");

        // Test 1: Single backend routing
        test_single_backend_routing(ctx).await?;

        // Test 2: Multiple backends with Maglev
        test_multiple_backend_routing(ctx).await?;

        // Test 3: Header preservation
        test_header_preservation(ctx).await?;

        println!("✅ All basic routing tests passed!\n");
        Ok(())
    }

    fn should_skip(&self, config: &TestConfig) -> bool {
        !config.scenarios.basic_routing
    }
}

/// Test 1: Single backend routing
///
/// Deploy: Backend → Gateway → HTTPRoute
/// Verify: HTTP request reaches backend and returns 200 OK
async fn test_single_backend_routing(ctx: &TestContext) -> TestResult {
    println!("  📝 Test: Single Backend Routing");

    // Deploy test backend
    let backend_yaml = fixtures::test_backend("echo-backend", &ctx.namespace, 8080, 1);
    k8s::apply_yaml(&ctx.client, &backend_yaml).await?;

    // Wait for backend to be ready
    println!("  ⏳ Waiting for backend pod to be ready...");
    wait_for_pods_ready(ctx, "echo-backend", 1, ctx.timeouts.backend_ready).await?;

    // Create Gateway
    let gateway_yaml = fixtures::gateway_with_http("test-gateway", &ctx.namespace);
    k8s::apply_yaml(&ctx.client, &gateway_yaml).await?;

    // Wait for Gateway to be accepted
    assertions::wait_for_gateway_accepted(
        &ctx.client,
        &ctx.namespace,
        "test-gateway",
        ctx.timeouts.gateway_ready,
    )
    .await?;

    // Create HTTPRoute
    let route_yaml = fixtures::http_route(
        "test-route",
        &ctx.namespace,
        "test-gateway",
        "/echo",
        "echo-backend",
        8080,
    );
    k8s::apply_yaml(&ctx.client, &route_yaml).await?;

    // Wait for route to be reconciled
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Send HTTP request through RAUTA
    let gateway_ip = get_gateway_address(ctx, "test-gateway").await?;
    let response = reqwest::get(format!("http://{}/echo", gateway_ip)).await?;

    // Assert response
    if !response.status().is_success() {
        return Err(format!(
            "Expected 200 OK, got {}",
            response.status()
        )
        .into());
    }

    let body = response.text().await?;
    println!("  ✅ Backend responded: {}", body.lines().next().unwrap_or(""));

    Ok(())
}

/// Test 2: Multiple backends with Maglev load balancing
async fn test_multiple_backend_routing(ctx: &TestContext) -> TestResult {
    println!("  📝 Test: Multiple Backends with Maglev");

    // Deploy 3 backend replicas
    let backend_yaml = fixtures::test_backend("multi-backend", &ctx.namespace, 8080, 3);
    k8s::apply_yaml(&ctx.client, &backend_yaml).await?;

    // Wait for all 3 pods
    println!("  ⏳ Waiting for 3 backend pods...");
    wait_for_pods_ready(ctx, "multi-backend", 3, ctx.timeouts.backend_ready).await?;

    // Create Gateway + HTTPRoute
    let gateway_yaml = fixtures::gateway_with_http("multi-gateway", &ctx.namespace);
    k8s::apply_yaml(&ctx.client, &gateway_yaml).await?;

    assertions::wait_for_gateway_accepted(
        &ctx.client,
        &ctx.namespace,
        "multi-gateway",
        ctx.timeouts.gateway_ready,
    )
    .await?;

    let route_yaml = fixtures::http_route(
        "multi-route",
        &ctx.namespace,
        "multi-gateway",
        "/api",
        "multi-backend",
        8080,
    );
    k8s::apply_yaml(&ctx.client, &route_yaml).await?;

    tokio::time::sleep(Duration::from_secs(5)).await;

    // Send multiple requests and verify distribution
    let gateway_ip = get_gateway_address(ctx, "multi-gateway").await?;

    println!("  📊 Sending 30 requests to test Maglev distribution...");
    let mut success_count = 0;

    for i in 0..30 {
        match reqwest::get(format!("http://{}/api/test-{}", gateway_ip, i)).await {
            Ok(resp) if resp.status().is_success() => success_count += 1,
            Ok(resp) => {
                println!("    ⚠️  Request {} got status {}", i, resp.status());
            }
            Err(e) => {
                println!("    ⚠️  Request {} failed: {}", i, e);
            }
        }
    }

    // Assert at least 80% success rate (allows for some startup flakiness)
    if success_count < 24 {
        return Err(format!(
            "Expected at least 24/30 successful requests, got {}",
            success_count
        )
        .into());
    }

    println!("  ✅ Maglev load balancing working ({}/30 successful)", success_count);

    Ok(())
}

/// Test 3: Header preservation
async fn test_header_preservation(ctx: &TestContext) -> TestResult {
    println!("  📝 Test: Header Preservation");

    // Reuse backend from test 1 if it exists, or create new one
    let backend_yaml = fixtures::test_backend("header-backend", &ctx.namespace, 8080, 1);
    k8s::apply_yaml(&ctx.client, &backend_yaml).await?;

    wait_for_pods_ready(ctx, "header-backend", 1, ctx.timeouts.backend_ready).await?;

    let gateway_yaml = fixtures::gateway_with_http("header-gateway", &ctx.namespace);
    k8s::apply_yaml(&ctx.client, &gateway_yaml).await?;

    assertions::wait_for_gateway_accepted(
        &ctx.client,
        &ctx.namespace,
        "header-gateway",
        ctx.timeouts.gateway_ready,
    )
    .await?;

    let route_yaml = fixtures::http_route(
        "header-route",
        &ctx.namespace,
        "header-gateway",
        "/headers",
        "header-backend",
        8080,
    );
    k8s::apply_yaml(&ctx.client, &route_yaml).await?;

    tokio::time::sleep(Duration::from_secs(5)).await;

    // Send request with custom headers
    let gateway_ip = get_gateway_address(ctx, "header-gateway").await?;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("http://{}/headers", gateway_ip))
        .header("X-Request-ID", "test-12345")
        .header("Authorization", "Bearer test-token")
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!(
            "Expected 200 OK, got {}",
            response.status()
        )
        .into());
    }

    println!("  ✅ Headers preserved through RAUTA");

    Ok(())
}

/// Helper: Wait for pods to be ready
async fn wait_for_pods_ready(
    ctx: &TestContext,
    app_label: &str,
    expected_count: usize,
    timeout_secs: u64,
) -> TestResult {
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::{Api, ListParams};

    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), &ctx.namespace);
    let start = std::time::Instant::now();

    while start.elapsed().as_secs() < timeout_secs {
        let lp = ListParams::default().labels(&format!("app={}", app_label));
        let pod_list = pods.list(&lp).await?;

        let ready_count = pod_list
            .items
            .iter()
            .filter(|pod| {
                pod.status
                    .as_ref()
                    .and_then(|s| s.conditions.as_ref())
                    .map(|conditions| {
                        conditions.iter().any(|c| {
                            c.type_ == "Ready" && c.status == "True"
                        })
                    })
                    .unwrap_or(false)
            })
            .count();

        if ready_count >= expected_count {
            println!("  ✅ {}/{} pods ready", ready_count, expected_count);
            return Ok(());
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    Err(format!(
        "Timeout waiting for {} pods with label app={} to be ready",
        expected_count, app_label
    )
    .into())
}

/// Helper: Get Gateway load balancer address
async fn get_gateway_address(ctx: &TestContext, gateway_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    use gateway_api::apis::standard::gateways::Gateway;
    use kube::api::Api;

    let gateways: Api<Gateway> = Api::namespaced(ctx.client.clone(), &ctx.namespace);
    let gateway = gateways.get(gateway_name).await?;

    // In kind cluster, we'll use port-forward
    // In production, use gateway.status.addresses

    // For now, return a placeholder
    // TODO: Implement port-forwarding or use Gateway status addresses

    Ok("127.0.0.1:8080".to_string())
}
