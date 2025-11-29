//! Kind (Kubernetes IN Docker) cluster provider

use crate::integration::framework::cluster_provider::{ClusterInfo, ClusterProvider};
use async_trait::async_trait;
use std::process::Command;

pub struct KindProvider {
    cluster_name: String,
    worker_nodes: u32,
}

impl KindProvider {
    pub fn new(cluster_name: String, worker_nodes: u32) -> Self {
        Self {
            cluster_name,
            worker_nodes,
        }
    }

    /// Check if cluster already exists
    fn cluster_exists(&self) -> Result<bool, Box<dyn std::error::Error>> {
        let output = Command::new("kind").args(&["get", "clusters"]).output()?;

        let clusters = String::from_utf8_lossy(&output.stdout);
        Ok(clusters.lines().any(|line| line == self.cluster_name))
    }

    /// Install Gateway API CRDs
    async fn install_gateway_api_crds(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("  📦 Installing Gateway API CRDs...");

        let output = Command::new("kubectl")
            .args(&[
                "apply",
                "-f",
                "https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.0.0/standard-install.yaml",
            ])
            .output()?;

        if !output.status.success() {
            return Err(format!(
                "Failed to install Gateway API CRDs: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        Ok(())
    }

    /// Load a Docker image into the Kind cluster
    async fn load_image(&self, image: &str) -> Result<(), Box<dyn std::error::Error>> {
        println!(
            "  📦 Loading image {} into cluster {}...",
            image, self.cluster_name
        );

        let output = Command::new("kind")
            .args(&["load", "docker-image", image, "--name", &self.cluster_name])
            .output()?;

        if !output.status.success() {
            return Err(format!(
                "Failed to load image: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        Ok(())
    }
}

#[async_trait]
impl ClusterProvider for KindProvider {
    async fn create(&self) -> Result<ClusterInfo, Box<dyn std::error::Error>> {
        println!("🔧 Creating Kind cluster: {}", self.cluster_name);

        // Check if cluster already exists
        if self.cluster_exists()? {
            println!("✅ Cluster {} already exists, reusing", self.cluster_name);
            return Ok(ClusterInfo {
                name: self.cluster_name.clone(),
                kubeconfig_context: format!("kind-{}", self.cluster_name),
                api_endpoint: "https://127.0.0.1:6443".to_string(), // Kind default
                created: false,
            });
        }

        // Generate Kind config with control plane + worker nodes
        let mut nodes_yaml = String::from("- role: control-plane\n");
        for _ in 0..self.worker_nodes {
            nodes_yaml.push_str("- role: worker\n");
        }

        let config = format!(
            r#"kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
{}networking:
  disableDefaultCNI: false
  podSubnet: "10.244.0.0/16"
"#,
            nodes_yaml
        );

        let config_path = format!("/tmp/kind-config-{}.yaml", self.cluster_name);
        std::fs::write(&config_path, config)?;

        // Create cluster
        println!(
            "  🏗️  Creating cluster with {} worker nodes...",
            self.worker_nodes
        );
        let output = Command::new("kind")
            .args(&[
                "create",
                "cluster",
                "--name",
                &self.cluster_name,
                "--config",
                &config_path,
            ])
            .output()?;

        if !output.status.success() {
            return Err(format!(
                "Failed to create cluster: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        // Install Gateway API CRDs
        self.install_gateway_api_crds().await?;

        println!("✅ Cluster {} created successfully", self.cluster_name);

        Ok(ClusterInfo {
            name: self.cluster_name.clone(),
            kubeconfig_context: format!("kind-{}", self.cluster_name),
            api_endpoint: "https://127.0.0.1:6443".to_string(),
            created: true,
        })
    }

    async fn delete(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("🗑️  Deleting Kind cluster: {}", self.cluster_name);

        let output = Command::new("kind")
            .args(&["delete", "cluster", "--name", &self.cluster_name])
            .output()?;

        if !output.status.success() {
            return Err(format!(
                "Failed to delete cluster: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        println!("✅ Cluster {} deleted", self.cluster_name);
        Ok(())
    }

    fn get_kubeconfig_context(&self) -> Result<String, Box<dyn std::error::Error>> {
        Ok(format!("kind-{}", self.cluster_name))
    }

    async fn deploy_rauta(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("🚀 Deploying RAUTA controller to cluster");

        // Find project root (where Cargo.toml is)
        let project_root = std::env::var("CARGO_MANIFEST_DIR")
            .map(|p| std::path::PathBuf::from(p).parent().unwrap().to_path_buf())
            .unwrap_or_else(|_| std::env::current_dir().unwrap());

        // Build RAUTA Docker image
        println!("  🔨 Building RAUTA Docker image...");
        let output = Command::new("docker")
            .current_dir(&project_root)
            .args(&[
                "build",
                "-t",
                "rauta:test",
                "-f",
                "docker/Dockerfile.control-local",
                ".",
            ])
            .output()?;

        if !output.status.success() {
            return Err(format!(
                "Failed to build RAUTA image: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        // Load image into Kind
        self.load_image("rauta:test").await?;

        // Create RAUTA deployment YAML
        let deployment_yaml = r#"apiVersion: v1
kind: ServiceAccount
metadata:
  name: rauta
  namespace: default
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: rauta
rules:
- apiGroups: [""]
  resources: ["services", "endpoints", "secrets"]
  verbs: ["get", "list", "watch"]
- apiGroups: ["discovery.k8s.io"]
  resources: ["endpointslices"]
  verbs: ["get", "list", "watch"]
- apiGroups: ["gateway.networking.k8s.io"]
  resources: ["gatewayclasses", "gateways", "httproutes"]
  verbs: ["get", "list", "watch", "update", "patch"]
- apiGroups: ["gateway.networking.k8s.io"]
  resources: ["gatewayclasses/status", "gateways/status", "httproutes/status"]
  verbs: ["update", "patch"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: rauta
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: rauta
subjects:
- kind: ServiceAccount
  name: rauta
  namespace: default
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: rauta
  namespace: default
spec:
  replicas: 1
  selector:
    matchLabels:
      app: rauta
  template:
    metadata:
      labels:
        app: rauta
    spec:
      serviceAccountName: rauta
      containers:
      - name: rauta
        image: rauta:test
        imagePullPolicy: Never
        env:
        - name: RAUTA_K8S_MODE
          value: "true"
        - name: RAUTA_GATEWAY_CLASS
          value: "rauta"
        - name: RAUTA_LOG_LEVEL
          value: "info"
"#;

        // Write deployment YAML to temp file
        let temp_file = format!("/tmp/rauta-deployment-{}.yaml", self.cluster_name);
        std::fs::write(&temp_file, deployment_yaml)?;

        // Apply deployment
        println!("  ⚙️  Applying RAUTA deployment...");
        let output = Command::new("kubectl")
            .args(&["apply", "-f", &temp_file])
            .output()?;

        if !output.status.success() {
            return Err(format!(
                "Failed to deploy RAUTA: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        // Wait for RAUTA to be ready
        println!("  ⏳ Waiting for RAUTA pod to be ready...");
        let output = Command::new("kubectl")
            .args(&[
                "wait",
                "--for=condition=ready",
                "pod",
                "-l",
                "app=rauta",
                "--timeout=60s",
            ])
            .output()?;

        if !output.status.success() {
            return Err(format!(
                "RAUTA pod failed to become ready: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        println!("✅ RAUTA controller deployed and ready");
        Ok(())
    }

    fn name(&self) -> &str {
        &self.cluster_name
    }
}
