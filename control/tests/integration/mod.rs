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

// Allow clippy warnings for test code
#![allow(
    dead_code,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_borrows_for_generic_args,
    clippy::useless_format,
    clippy::to_string_in_format_args,
    clippy::map_clone,
    clippy::unwrap_used,
    clippy::upper_case_acronyms
)]

pub mod framework;
pub mod scenarios;

pub use framework::{TestContext, TestResult};
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

#[derive(Debug, Clone, Deserialize)]
pub struct ClusterConfig {
    #[serde(rename = "type")]
    pub cluster_type: String, // "kind", "k3s", "eks", "aks", "gke", "existing"
    pub name: String,
    pub reuse: bool,
    pub cleanup: bool,

    // Provider-specific configs
    pub kind: Option<KindClusterConfig>,
    pub k3s: Option<K3sClusterConfig>,
    pub eks: Option<EKSClusterConfig>,
    pub aks: Option<AKSClusterConfig>,
    pub gke: Option<GKEClusterConfig>,
    pub existing: Option<ExistingClusterConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KindClusterConfig {
    pub worker_nodes: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct K3sClusterConfig {
    pub nodes: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EKSClusterConfig {
    pub region: String,
    pub node_count: u32,
    pub instance_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AKSClusterConfig {
    pub resource_group: String,
    pub node_count: u32,
    pub vm_size: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GKEClusterConfig {
    pub project: String,
    pub zone: String,
    pub node_count: u32,
    pub machine_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExistingClusterConfig {
    pub kubeconfig_path: String,
    pub context: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioConfig {
    pub tls_validation: bool,
    pub basic_routing: bool,
    pub endpointslice_updates: bool,
    pub lock_poisoning: bool,
    pub load_testing: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimeoutConfig {
    pub gateway_ready: u64,
    pub route_ready: u64,
    pub backend_ready: u64,
    pub reconciliation: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PerformanceConfig {
    pub tool: String,
    pub duration_seconds: u64,
    pub connections: u64,
    pub threads: u64,
    pub target_rps: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SnifferConfig {
    pub enabled: bool,
    pub interface: String,
    pub filter: String,
    pub output_dir: String,
}

impl ClusterConfig {
    /// Convert configuration to ClusterType enum
    pub fn to_cluster_type(&self) -> Result<framework::cluster_provider::ClusterType, String> {
        use framework::cluster_provider::ClusterType;

        match self.cluster_type.as_str() {
            "kind" => {
                let kind_config = self
                    .kind
                    .as_ref()
                    .ok_or("Missing [cluster.kind] configuration")?;
                Ok(ClusterType::Kind {
                    name: self.name.clone(),
                    nodes: kind_config.worker_nodes,
                })
            }
            "k3s" => {
                let k3s_config = self
                    .k3s
                    .as_ref()
                    .ok_or("Missing [cluster.k3s] configuration")?;
                Ok(ClusterType::K3s {
                    name: self.name.clone(),
                    nodes: k3s_config.nodes,
                })
            }
            "eks" => {
                let eks_config = self
                    .eks
                    .as_ref()
                    .ok_or("Missing [cluster.eks] configuration")?;
                Ok(ClusterType::EKS {
                    region: eks_config.region.clone(),
                    cluster_name: self.name.clone(),
                    node_count: eks_config.node_count,
                    instance_type: eks_config.instance_type.clone(),
                })
            }
            "aks" => {
                let aks_config = self
                    .aks
                    .as_ref()
                    .ok_or("Missing [cluster.aks] configuration")?;
                Ok(ClusterType::AKS {
                    resource_group: aks_config.resource_group.clone(),
                    cluster_name: self.name.clone(),
                    node_count: aks_config.node_count,
                    vm_size: aks_config.vm_size.clone(),
                })
            }
            "gke" => {
                let gke_config = self
                    .gke
                    .as_ref()
                    .ok_or("Missing [cluster.gke] configuration")?;
                Ok(ClusterType::GKE {
                    project: gke_config.project.clone(),
                    zone: gke_config.zone.clone(),
                    cluster_name: self.name.clone(),
                    node_count: gke_config.node_count,
                    machine_type: gke_config.machine_type.clone(),
                })
            }
            "existing" => {
                let existing_config = self
                    .existing
                    .as_ref()
                    .ok_or("Missing [cluster.existing] configuration")?;
                Ok(ClusterType::Existing {
                    kubeconfig_path: existing_config.kubeconfig_path.clone(),
                    context: existing_config.context.clone(),
                })
            }
            _ => Err(format!("Unknown cluster type: {}", self.cluster_type)),
        }
    }
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
#[async_trait::async_trait]
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
