//! RAUTA Integration Test Framework
//!
//! A comprehensive testing framework for validating Gateway API implementation,
//! performance, and failure modes.
//!
//! ## Architecture
//!
//! - **framework/**: Core infrastructure (cluster management, K8s client, assertions)
//! - **scenarios/**: Test scenarios that can be enabled/disabled via config.toml
//! - **fixtures/**: Reusable K8s resources (Secrets, Gateways, HTTPRoutes)
//!
//! ## Running Tests
//!
//! ```bash
//! # Run all enabled scenarios
//! cargo test --test integration
//!
//! # Run specific scenario
//! cargo test --test integration tls_validation
//!
//! # Enable performance tests
//! # Edit tests/integration/config.toml: load_testing = true
//! cargo test --test integration -- --ignored
//! ```
//!
//! ## Configuration
//!
//! Edit `tests/integration/config.toml` to:
//! - Enable/disable test scenarios
//! - Configure cluster reuse and cleanup
//! - Set timeouts
//! - Configure performance testing
//! - Enable network sniffing

mod framework;
mod scenarios;

use framework::{TestContext, TestResult};
use serde::Deserialize;
use std::fs;

/// Test configuration loaded from config.toml
#[derive(Debug, Deserialize)]
pub struct TestConfig {
    pub cluster: ClusterConfig,
    pub scenarios: ScenarioConfig,
    pub timeouts: TimeoutConfig,
    pub performance: PerformanceConfig,
    pub sniffer: SnifferConfig,
}

#[derive(Debug, Deserialize)]
pub struct ClusterConfig {
    pub name: String,
    pub reuse: bool,
    pub cleanup: bool,
}

#[derive(Debug, Deserialize)]
pub struct ScenarioConfig {
    pub tls_validation: bool,
    pub basic_routing: bool,
    pub endpointslice_updates: bool,
    pub lock_poisoning: bool,
    pub load_testing: bool,
}

#[derive(Debug, Deserialize)]
pub struct TimeoutConfig {
    pub gateway_ready: u64,
    pub route_ready: u64,
    pub backend_ready: u64,
    pub reconciliation: u64,
}

#[derive(Debug, Deserialize)]
pub struct PerformanceConfig {
    pub tool: String,
    pub duration_seconds: u64,
    pub connections: u64,
    pub threads: u64,
    pub target_rps: u64,
}

#[derive(Debug, Deserialize)]
pub struct SnifferConfig {
    pub enabled: bool,
    pub interface: String,
    pub filter: String,
    pub output_dir: String,
}

impl TestConfig {
    /// Load configuration from config.toml
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let config_path = "tests/integration/config.toml";
        let contents = fs::read_to_string(config_path)?;
        let config: TestConfig = toml::from_str(&contents)?;
        Ok(config)
    }
}

/// Test scenario trait
///
/// Each test scenario implements this trait to integrate with the framework.
pub trait TestScenario {
    /// Scenario name (for logging and filtering)
    fn name(&self) -> &str;

    /// Run the test scenario
    async fn run(&self, ctx: &mut TestContext) -> TestResult;

    /// Whether this scenario should be skipped based on config
    fn should_skip(&self, config: &TestConfig) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_loads() {
        let config = TestConfig::load().expect("Failed to load config.toml");
        assert_eq!(config.cluster.name, "rauta-test");
    }
}
