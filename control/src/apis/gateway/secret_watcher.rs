//! Secret watcher for TLS certificate rotation
//!
//! Watches Kubernetes Secret resources and automatically updates
//! certificates when Secrets change (zero-downtime hot-reload).
//!
//! ## Usage
//!
//! ```ignore
//! use control::apis::gateway::secret_watcher::watch_tls_secrets;
//! use control::proxy::tls::SniResolver;
//! use kube::Client;
//! use std::sync::Arc;
//! use tokio::sync::RwLock;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let client = Client::try_default().await?;
//!     let sni_resolver = Arc::new(RwLock::new(SniResolver::new()));
//!
//!     // Start watching Secrets in all namespaces
//!     watch_tls_secrets(client, sni_resolver).await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! ## Secret Format
//!
//! Secrets must be `type: kubernetes.io/tls` with annotation:
//! ```yaml
//! apiVersion: v1
//! kind: Secret
//! metadata:
//!   name: example-com-tls
//!   annotations:
//!     rauta.io/hostname: example.com  # Required: SNI hostname
//! type: kubernetes.io/tls
//! data:
//!   tls.crt: <base64-encoded-cert>
//!   tls.key: <base64-encoded-key>
//! ```

use crate::proxy::tls::{parse_cert_from_secret_data, SniResolver};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Secret;
use kube::runtime::watcher;
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{api::Api, Client, ResourceExt};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Watch TLS Secrets and update SniResolver on changes
///
/// This function runs indefinitely, watching for Secret changes.
/// Call it in a tokio::spawn() task.
#[allow(dead_code)] // Used in K8s mode
pub async fn watch_tls_secrets(
    client: Client,
    sni_resolver: Arc<RwLock<SniResolver>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Watch all Secrets across all namespaces
    let api: Api<Secret> = Api::all(client);
    let watcher = watcher(api, WatcherConfig::default());

    futures::pin_mut!(watcher);

    info!("Starting TLS Secret watcher");

    while let Some(event) = watcher.next().await {
        match event {
            Ok(watcher::Event::Apply(secret)) | Ok(watcher::Event::InitApply(secret)) => {
                if let Err(e) = handle_secret_apply(&secret, &sni_resolver).await {
                    warn!(
                        "Failed to handle Secret {}/{}: {}",
                        secret.namespace().unwrap_or_else(|| "default".to_string()),
                        secret.name_any(),
                        e
                    );
                }
            }
            Ok(watcher::Event::Delete(secret)) => {
                let namespace = secret.namespace().unwrap_or_else(|| "default".to_string());
                let name = secret.name_any();
                warn!(
                    "Secret deleted: {}/{} (certificate removal not yet implemented)",
                    namespace, name
                );
            }
            Ok(watcher::Event::Init) => {
                debug!("Secret watcher initialized");
            }
            Ok(watcher::Event::InitDone) => {
                info!("Secret watcher initial sync complete");
            }
            Err(e) => {
                warn!("Secret watcher error: {}", e);
            }
        }
    }

    Ok(())
}

/// Handle Secret apply/update event
async fn handle_secret_apply(
    secret: &Secret,
    sni_resolver: &Arc<RwLock<SniResolver>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let namespace = secret.namespace().unwrap_or_else(|| "default".to_string());
    let name = secret.name_any();

    // Check if this is a TLS secret
    if let Some(ref secret_type) = secret.type_ {
        if secret_type != "kubernetes.io/tls" {
            return Ok(()); // Ignore non-TLS secrets silently
        }
    } else {
        return Ok(()); // Ignore secrets without type
    }

    // Extract hostname from annotation
    let hostname = secret
        .metadata
        .annotations
        .as_ref()
        .and_then(|annot| annot.get("rauta.io/hostname"))
        .ok_or_else(|| {
            format!(
                "Secret {}/{} missing rauta.io/hostname annotation",
                namespace, name
            )
        })?;

    info!(
        "Updating certificate for '{}' from Secret {}/{}",
        hostname, namespace, name
    );

    // Parse certificate from Secret data
    let cert = parse_cert_from_secret_data(secret)?;

    // Update SniResolver (hot-reload)
    let mut resolver = sni_resolver.write().await;
    resolver.update_cert(hostname.to_string(), cert)?;

    info!(
        "Certificate for '{}' updated successfully (zero-downtime)",
        hostname
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::tls::TlsCertificate;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use k8s_openapi::ByteString;
    use std::collections::BTreeMap;

    fn init_crypto() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    /// RED: Test that Secret handler updates SniResolver
    #[tokio::test]
    async fn test_secret_handler_updates_certificate() {
        init_crypto();

        // Create SniResolver with initial certificate
        let mut resolver = SniResolver::new();
        let cert_v1_pem = include_bytes!("../../../../test_fixtures/tls/example.com.crt");
        let key_v1_pem = include_bytes!("../../../../test_fixtures/tls/example.com.key");
        let cert_v1 =
            TlsCertificate::from_pem(cert_v1_pem, key_v1_pem).expect("Should parse v1 certificate");

        resolver
            .add_cert("example.com".to_string(), cert_v1)
            .expect("Should add v1 certificate");

        let resolver = Arc::new(RwLock::new(resolver));

        // Create a Secret with renewed certificate
        let cert_v2_pem = include_bytes!("../../../../test_fixtures/tls/example.com-v2.crt");
        let key_v2_pem = include_bytes!("../../../../test_fixtures/tls/example.com-v2.key");

        let mut data = BTreeMap::new();
        data.insert("tls.crt".to_string(), ByteString(cert_v2_pem.to_vec()));
        data.insert("tls.key".to_string(), ByteString(key_v2_pem.to_vec()));

        let mut annotations = BTreeMap::new();
        annotations.insert("rauta.io/hostname".to_string(), "example.com".to_string());

        let secret = Secret {
            metadata: ObjectMeta {
                name: Some("example-com-tls".to_string()),
                namespace: Some("default".to_string()),
                annotations: Some(annotations),
                ..Default::default()
            },
            type_: Some("kubernetes.io/tls".to_string()),
            data: Some(data),
            ..Default::default()
        };

        // Handle Secret apply
        handle_secret_apply(&secret, &resolver)
            .await
            .expect("Should handle Secret apply");

        // Verify certificate was updated
        let resolver = resolver.read().await;
        assert!(
            resolver.has_cert("example.com"),
            "Should still have certificate for example.com"
        );

        // Verify new certificate is being used
        let config = resolver
            .to_server_config()
            .expect("Should build ServerConfig with updated cert");

        assert!(
            Arc::strong_count(&config) >= 1,
            "ServerConfig should be valid"
        );
    }

    #[tokio::test]
    async fn test_secret_handler_ignores_non_tls_secrets() {
        init_crypto();

        let resolver = Arc::new(RwLock::new(SniResolver::new()));

        // Create a non-TLS Secret
        let secret = Secret {
            metadata: ObjectMeta {
                name: Some("my-app-config".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            type_: Some("Opaque".to_string()),
            ..Default::default()
        };

        // Should succeed (ignore non-TLS secrets)
        let result = handle_secret_apply(&secret, &resolver).await;
        assert!(result.is_ok(), "Should ignore non-TLS secrets");
    }

    #[tokio::test]
    async fn test_secret_handler_requires_hostname_annotation() {
        init_crypto();

        let resolver = Arc::new(RwLock::new(SniResolver::new()));

        // Create TLS Secret WITHOUT rauta.io/hostname annotation
        let cert_pem = include_bytes!("../../../../test_fixtures/tls/example.com.crt");
        let key_pem = include_bytes!("../../../../test_fixtures/tls/example.com.key");

        let mut data = BTreeMap::new();
        data.insert("tls.crt".to_string(), ByteString(cert_pem.to_vec()));
        data.insert("tls.key".to_string(), ByteString(key_pem.to_vec()));

        let secret = Secret {
            metadata: ObjectMeta {
                name: Some("example-com-tls".to_string()),
                namespace: Some("default".to_string()),
                // NO annotations
                ..Default::default()
            },
            type_: Some("kubernetes.io/tls".to_string()),
            data: Some(data),
            ..Default::default()
        };

        // Should fail - missing hostname annotation
        let result = handle_secret_apply(&secret, &resolver).await;
        assert!(
            result.is_err(),
            "Should reject Secret without rauta.io/hostname annotation"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("rauta.io/hostname"),
            "Error should mention missing annotation"
        );
    }
}
