//! Kind cluster management for integration testing

use std::process::Command;

/// Create a kind cluster
pub async fn create(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("🔧 Creating kind cluster: {}", name);

    // Check if cluster already exists
    let output = Command::new("kind").args(&["get", "clusters"]).output()?;

    let clusters = String::from_utf8_lossy(&output.stdout);
    if clusters.lines().any(|line| line == name) {
        println!("✅ Cluster {} already exists, reusing", name);
        return Ok(());
    }

    // Create cluster with custom config
    let config = format!(
        r#"
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
- role: control-plane
- role: worker
- role: worker
networking:
  disableDefaultCNI: false
  podSubnet: "10.244.0.0/16"
"#
    );

    std::fs::write("/tmp/kind-config.yaml", config)?;

    let output = Command::new("kind")
        .args(&[
            "create",
            "cluster",
            "--name",
            name,
            "--config",
            "/tmp/kind-config.yaml",
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
    println!("📦 Installing Gateway API CRDs");
    install_gateway_api_crds().await?;

    println!("✅ Cluster {} created successfully", name);
    Ok(())
}

/// Delete a kind cluster
pub async fn delete(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("🗑️  Deleting kind cluster: {}", name);

    let output = Command::new("kind")
        .args(&["delete", "cluster", "--name", name])
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "Failed to delete cluster: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    println!("✅ Cluster {} deleted", name);
    Ok(())
}

/// Install Gateway API CRDs
async fn install_gateway_api_crds() -> Result<(), Box<dyn std::error::Error>> {
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

/// Load a Docker image into the kind cluster
pub async fn load_image(cluster_name: &str, image: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("📦 Loading image {} into cluster {}", image, cluster_name);

    let output = Command::new("kind")
        .args(&["load", "docker-image", image, "--name", cluster_name])
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

/// Deploy RAUTA controller to the cluster
pub async fn deploy_rauta(cluster_name: &str) -> Result<(), Box<dyn std::error::Error>> {
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
    println!("  📦 Loading RAUTA image into Kind cluster...");
    load_image(cluster_name, "rauta:test").await?;

    // Create RAUTA deployment YAML
    let deployment_yaml = r#"
apiVersion: v1
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
    let temp_file = "/tmp/rauta-deployment.yaml";
    std::fs::write(temp_file, deployment_yaml)?;

    // Apply deployment
    println!("  ⚙️  Applying RAUTA deployment...");
    let output = Command::new("kubectl")
        .args(&["apply", "-f", temp_file])
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
