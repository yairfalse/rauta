//! Kubernetes client helpers for integration testing

use k8s_openapi::api::core::v1::{Namespace, Secret};
use kube::api::{Api, DeleteParams, PostParams};
use kube::Client;
use std::collections::BTreeMap;

/// Create a Kubernetes client using the current context
pub async fn create_client() -> Result<Client, Box<dyn std::error::Error>> {
    let client = Client::try_default().await?;
    Ok(client)
}

/// Create a namespace
pub async fn create_namespace(
    client: &Client,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let namespaces: Api<Namespace> = Api::all(client.clone());

    let ns = Namespace {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(name.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };

    namespaces.create(&PostParams::default(), &ns).await?;
    println!("✅ Created namespace: {}", name);

    Ok(())
}

/// Delete a namespace
pub async fn delete_namespace(
    client: &Client,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let namespaces: Api<Namespace> = Api::all(client.clone());

    namespaces.delete(name, &DeleteParams::default()).await?;

    println!("✅ Deleted namespace: {}", name);
    Ok(())
}

/// Create a TLS Secret
pub async fn create_tls_secret(
    client: &Client,
    namespace: &str,
    name: &str,
    cert: &[u8],
    key: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);

    let mut data = BTreeMap::new();
    data.insert(
        "tls.crt".to_string(),
        k8s_openapi::ByteString(cert.to_vec()),
    );
    data.insert("tls.key".to_string(), k8s_openapi::ByteString(key.to_vec()));

    let secret = Secret {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        data: Some(data),
        type_: Some("kubernetes.io/tls".to_string()),
        ..Default::default()
    };

    secrets.create(&PostParams::default(), &secret).await?;
    println!("✅ Created TLS secret: {}/{}", namespace, name);

    Ok(())
}

/// Create a malformed Secret (missing tls.key)
pub async fn create_malformed_secret(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);

    let mut data = BTreeMap::new();
    // Only tls.crt, missing tls.key
    data.insert(
        "tls.crt".to_string(),
        k8s_openapi::ByteString(b"fake-cert".to_vec()),
    );

    let secret = Secret {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };

    secrets.create(&PostParams::default(), &secret).await?;
    println!("✅ Created malformed secret: {}/{}", namespace, name);

    Ok(())
}

/// Apply a YAML manifest
pub async fn apply_yaml(_client: &Client, yaml: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Write to temp file
    let temp_file = format!("/tmp/rauta-test-{}.yaml", uuid::Uuid::new_v4());
    std::fs::write(&temp_file, yaml)?;

    // Apply with kubectl
    let output = std::process::Command::new("kubectl")
        .args(&["apply", "-f", &temp_file])
        .output()?;

    std::fs::remove_file(&temp_file)?;

    if !output.status.success() {
        return Err(format!(
            "Failed to apply YAML: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    Ok(())
}
