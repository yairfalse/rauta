//! Kind cluster management for integration testing

use std::process::Command;

/// Create a kind cluster
pub async fn create(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("🔧 Creating kind cluster: {}", name);

    // Check if cluster already exists
    let output = Command::new("kind")
        .args(&["get", "clusters"])
        .output()?;

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
