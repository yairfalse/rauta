//! Test framework core infrastructure

pub mod cluster;
pub mod k8s;
pub mod fixtures;
pub mod assertions;
pub mod metrics;
pub mod sniffer;

use std::time::Duration;

/// Test result type
pub type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Test context holding cluster state and clients
pub struct TestContext {
    /// K8s client
    pub client: kube::Client,

    /// Cluster name
    pub cluster_name: String,

    /// Test namespace
    pub namespace: String,

    /// Network sniffer (if enabled)
    pub sniffer: Option<sniffer::Sniffer>,

    /// Metrics collector
    pub metrics: metrics::MetricsCollector,

    /// Test timeouts
    pub timeouts: super::TimeoutConfig,
}

impl TestContext {
    /// Create a new test context
    pub async fn new(config: &super::TestConfig) -> Result<Self, Box<dyn std::error::Error>> {
        // Setup cluster
        let cluster_name = config.cluster.name.clone();

        if !config.cluster.reuse {
            cluster::create(&cluster_name).await?;
        }

        // Create K8s client
        let client = k8s::create_client().await?;

        // Create test namespace
        let namespace = format!("rauta-test-{}", uuid::Uuid::new_v4().to_string()[..8].to_string());
        k8s::create_namespace(&client, &namespace).await?;

        // Initialize sniffer if enabled
        let sniffer = if config.sniffer.enabled {
            Some(sniffer::Sniffer::new(&config.sniffer)?)
        } else {
            None
        };

        // Initialize metrics collector
        let metrics = metrics::MetricsCollector::new();

        Ok(Self {
            client,
            cluster_name,
            namespace,
            sniffer,
            metrics,
            timeouts: config.timeouts.clone(),
        })
    }

    /// Wait for a condition with timeout
    pub async fn wait_for<F>(&self, timeout: Duration, mut condition: F) -> TestResult
    where
        F: FnMut() -> bool,
    {
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            if condition() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        Err(format!("Timeout after {:?} waiting for condition", timeout).into())
    }

    /// Cleanup test resources
    pub async fn cleanup(&mut self, config: &super::TestConfig) -> TestResult {
        // Delete namespace
        k8s::delete_namespace(&self.client, &self.namespace).await?;

        // Stop sniffer
        if let Some(ref mut sniffer) = self.sniffer {
            sniffer.stop()?;
        }

        // Delete cluster if requested
        if config.cluster.cleanup && !config.cluster.reuse {
            cluster::delete(&self.cluster_name).await?;
        }

        Ok(())
    }
}
