//! RAUTA Integration Tests
//!
//! Run with: cargo test --test integration_test

mod integration;

use integration::{TestConfig, TestContext, TestScenario};
use integration::scenarios::tls_validation::TlsValidationScenario;
use integration::scenarios::basic_routing::BasicRoutingScenario;
use integration::scenarios::endpointslice::EndpointSliceScenario;
use integration::scenarios::load_test::LoadTestScenario;

#[tokio::test]
async fn run_integration_tests() {
    // Load configuration
    let config = TestConfig::load().expect("Failed to load test config");

    // Create test context (cluster + clients)
    let mut ctx = TestContext::new(&config)
        .await
        .expect("Failed to create test context");

    // Register test scenarios
    let scenarios: Vec<Box<dyn TestScenario>> = vec![
        Box::new(TlsValidationScenario),
        Box::new(BasicRoutingScenario),
        Box::new(EndpointSliceScenario),
        Box::new(LoadTestScenario),
    ];

    // Run enabled scenarios
    let mut passed = 0;
    let mut failed = 0;

    for scenario in scenarios {
        if scenario.should_skip(&config) {
            println!("⏭️  Skipping scenario: {}", scenario.name());
            continue;
        }

        println!("🏃 Running scenario: {}", scenario.name());

        match scenario.run(&mut ctx).await {
            Ok(()) => {
                println!("✅ Scenario passed: {}\n", scenario.name());
                passed += 1;
            }
            Err(e) => {
                eprintln!("❌ Scenario failed: {}", scenario.name());
                eprintln!("   Error: {}\n", e);
                failed += 1;
            }
        }
    }

    // Cleanup
    ctx.cleanup(&config)
        .await
        .expect("Failed to cleanup test resources");

    // Report results
    println!("📊 Test Summary:");
    println!("   Passed: {}", passed);
    println!("   Failed: {}", failed);

    if failed > 0 {
        panic!("{} integration test(s) failed", failed);
    }
}
