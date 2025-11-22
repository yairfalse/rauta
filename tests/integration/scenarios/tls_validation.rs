//! TLS Certificate Validation Test Scenarios
//!
//! Tests Gateway API TLS certificateRef validation according to spec.

use crate::framework::{TestContext, TestResult};
use crate::framework::{assertions, fixtures, k8s};
use crate::{TestConfig, TestScenario};

pub struct TlsValidationScenario;

impl TestScenario for TlsValidationScenario {
    fn name(&self) -> &str {
        "tls_validation"
    }

    async fn run(&self, ctx: &mut TestContext) -> TestResult {
        println!("\n🧪 Running TLS Validation Scenarios");

        // Test 1: Valid TLS Secret
        test_valid_tls_secret(ctx).await?;

        // Test 2: Nonexistent Secret
        test_nonexistent_secret(ctx).await?;

        // Test 3: Malformed Secret (missing tls.key)
        test_malformed_secret(ctx).await?;

        println!("✅ All TLS validation tests passed!\n");
        Ok(())
    }

    fn should_skip(&self, config: &TestConfig) -> bool {
        !config.scenarios.tls_validation
    }
}

/// Test 1: Gateway with valid TLS Secret → ResolvedRefs=True
async fn test_valid_tls_secret(ctx: &TestContext) -> TestResult {
    println!("  📝 Test: Valid TLS Secret");

    // Generate test certificate
    let (cert, key) = fixtures::generate_test_cert();

    // Create valid TLS Secret
    k8s::create_tls_secret(&ctx.client, &ctx.namespace, "valid-cert", &cert, &key).await?;

    // Create Gateway referencing valid Secret
    let gateway_yaml = fixtures::gateway_with_https("test-gateway-valid", &ctx.namespace, "valid-cert");
    k8s::apply_yaml(&ctx.client, &gateway_yaml).await?;

    // Wait for Gateway to be accepted
    assertions::wait_for_gateway_accepted(
        &ctx.client,
        &ctx.namespace,
        "test-gateway-valid",
        ctx.timeouts.gateway_ready,
    )
    .await?;

    // Assert listener has ResolvedRefs=True
    assertions::assert_listener_resolved_refs(
        &ctx.client,
        &ctx.namespace,
        "test-gateway-valid",
        "https",
        "True",
        "ResolvedRefs",
    )
    .await?;

    println!("  ✅ Valid TLS Secret accepted");
    Ok(())
}

/// Test 2: Gateway with nonexistent Secret → ResolvedRefs=False, "not found"
async fn test_nonexistent_secret(ctx: &TestContext) -> TestResult {
    println!("  📝 Test: Nonexistent TLS Secret");

    // Create Gateway referencing nonexistent Secret
    let gateway_yaml =
        fixtures::gateway_with_https("test-gateway-notfound", &ctx.namespace, "nonexistent-secret");
    k8s::apply_yaml(&ctx.client, &gateway_yaml).await?;

    // Wait for Gateway to be processed
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // Assert listener has ResolvedRefs=False
    assertions::assert_listener_resolved_refs(
        &ctx.client,
        &ctx.namespace,
        "test-gateway-notfound",
        "https",
        "False",
        "InvalidCertificateRef",
    )
    .await?;

    // Assert error message contains "not found"
    assertions::assert_listener_message_contains(
        &ctx.client,
        &ctx.namespace,
        "test-gateway-notfound",
        "https",
        "not found",
    )
    .await?;

    println!("  ✅ Nonexistent Secret rejected with proper error");
    Ok(())
}

/// Test 3: Gateway with malformed Secret (missing tls.key) → ResolvedRefs=False
async fn test_malformed_secret(ctx: &TestContext) -> TestResult {
    println!("  📝 Test: Malformed TLS Secret (missing tls.key)");

    // Create malformed Secret
    k8s::create_malformed_secret(&ctx.client, &ctx.namespace, "malformed-cert").await?;

    // Create Gateway referencing malformed Secret
    let gateway_yaml =
        fixtures::gateway_with_https("test-gateway-malformed", &ctx.namespace, "malformed-cert");
    k8s::apply_yaml(&ctx.client, &gateway_yaml).await?;

    // Wait for Gateway to be processed
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // Assert listener has ResolvedRefs=False
    assertions::assert_listener_resolved_refs(
        &ctx.client,
        &ctx.namespace,
        "test-gateway-malformed",
        "https",
        "False",
        "InvalidCertificateRef",
    )
    .await?;

    // Assert error message contains "missing required fields"
    assertions::assert_listener_message_contains(
        &ctx.client,
        &ctx.namespace,
        "test-gateway-malformed",
        "https",
        "missing required fields",
    )
    .await?;

    println!("  ✅ Malformed Secret rejected with proper error");
    Ok(())
}
