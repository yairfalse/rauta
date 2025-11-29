//! Cluster provider abstraction for multi-cloud testing
//!
//! This module provides a pluggable interface for creating and managing
//! Kubernetes clusters across different providers (Kind, K3s, EKS, AKS, GKE).
//!
//! Design Goals:
//! - Extraction-ready for Seppo (standalone K8s testing framework)
//! - Support local (Kind, K3s) and cloud (EKS, AKS, GKE) clusters
//! - Reusable cluster state across test runs
//! - Minimal provider-specific logic in test code

use async_trait::async_trait;
use std::fmt;

/// Cluster provider trait - implemented by each cluster type
#[async_trait]
pub trait ClusterProvider: Send + Sync {
    /// Create a new cluster (idempotent - reuses if exists)
    async fn create(&self) -> Result<ClusterInfo, Box<dyn std::error::Error>>;

    /// Delete the cluster
    async fn delete(&self) -> Result<(), Box<dyn std::error::Error>>;

    /// Get cluster connection info (kubeconfig path or context)
    fn get_kubeconfig_context(&self) -> Result<String, Box<dyn std::error::Error>>;

    /// Deploy RAUTA controller to the cluster
    async fn deploy_rauta(&self) -> Result<(), Box<dyn std::error::Error>>;

    /// Get cluster name for display
    fn name(&self) -> &str;
}

/// Cluster information returned after creation
#[derive(Debug, Clone)]
pub struct ClusterInfo {
    pub name: String,
    pub kubeconfig_context: String,
    pub api_endpoint: String,
    pub created: bool, // true if newly created, false if reused
}

/// Cluster type configuration
#[derive(Debug, Clone)]
pub enum ClusterType {
    /// Local Kind cluster
    Kind {
        name: String,
        nodes: u32, // Number of worker nodes (1 control plane + N workers)
    },

    /// Local K3s cluster
    K3s { name: String, nodes: u32 },

    /// AWS EKS cluster
    EKS {
        region: String,
        cluster_name: String,
        node_count: u32,
        instance_type: String, // e.g., "t3.medium"
    },

    /// Azure AKS cluster
    AKS {
        resource_group: String,
        cluster_name: String,
        node_count: u32,
        vm_size: String, // e.g., "Standard_DS2_v2"
    },

    /// Google GKE cluster
    GKE {
        project: String,
        zone: String,
        cluster_name: String,
        node_count: u32,
        machine_type: String, // e.g., "e2-medium"
    },

    /// Use existing cluster (for CI/CD or pre-provisioned environments)
    Existing {
        kubeconfig_path: String,
        context: String,
    },
}

impl ClusterType {
    /// Create the appropriate provider for this cluster type
    pub fn provider(&self) -> Box<dyn ClusterProvider> {
        match self {
            ClusterType::Kind { name, nodes } => Box::new(
                super::providers::kind::KindProvider::new(name.clone(), *nodes),
            ),
            ClusterType::K3s { .. } => {
                unimplemented!("K3s provider not yet implemented (extraction TODO)")
            }
            ClusterType::EKS { .. } => {
                unimplemented!("EKS provider not yet implemented (extraction TODO)")
            }
            ClusterType::AKS { .. } => {
                unimplemented!("AKS provider not yet implemented (extraction TODO)")
            }
            ClusterType::GKE { .. } => {
                unimplemented!("GKE provider not yet implemented (extraction TODO)")
            }
            ClusterType::Existing { .. } => {
                unimplemented!("Existing cluster provider not yet implemented (extraction TODO)")
            }
        }
    }
}

impl fmt::Display for ClusterType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClusterType::Kind { name, nodes } => write!(f, "Kind({}, {} workers)", name, nodes),
            ClusterType::K3s { name, nodes } => write!(f, "K3s({}, {} nodes)", name, nodes),
            ClusterType::EKS {
                region,
                cluster_name,
                ..
            } => write!(f, "EKS({}, {})", region, cluster_name),
            ClusterType::AKS {
                resource_group,
                cluster_name,
                ..
            } => write!(f, "AKS({}, {})", resource_group, cluster_name),
            ClusterType::GKE {
                project,
                zone,
                cluster_name,
                ..
            } => write!(f, "GKE({}, {}, {})", project, zone, cluster_name),
            ClusterType::Existing { context, .. } => write!(f, "Existing({})", context),
        }
    }
}
