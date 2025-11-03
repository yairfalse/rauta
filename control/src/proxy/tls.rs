//! TLS termination with rustls and SNI support
//!
//! Implements Gateway API TLS termination:
//! - Load certificates from Kubernetes Secrets
//! - SNI-based certificate selection
//! - Integration with hyper HTTPS server

use rustls::ServerConfig;
use rustls_pemfile::{certs, private_key};
use std::io::{self, BufReader};
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

/// TLS configuration for a Gateway listener
#[allow(dead_code)] // Used in gateway controller (not yet integrated)
#[derive(Clone)]
pub struct TlsConfig {
    /// SNI hostname (e.g., "example.com")
    pub hostname: String,
    /// TLS mode (only Terminate supported for now)
    pub mode: TlsMode,
    /// Certificate and private key
    pub certificate: TlsCertificate,
}

/// TLS modes from Gateway API
#[allow(dead_code)] // Used in gateway controller (not yet integrated)
#[derive(Debug, Clone, PartialEq)]
pub enum TlsMode {
    /// Terminate TLS at the Gateway (decrypt traffic)
    Terminate,
    /// Passthrough TLS (not implemented)
    #[allow(dead_code)]
    Passthrough,
}

/// TLS certificate loaded from Kubernetes Secret
#[allow(dead_code)] // Used in tests and future gateway integration
#[derive(Clone)]
pub struct TlsCertificate {
    /// Certificate chain (PEM format)
    pub cert_chain: Vec<u8>,
    /// Private key (PEM format)
    pub private_key: Vec<u8>,
}

#[allow(dead_code)] // Used in tests and future gateway integration
impl TlsCertificate {
    /// Create a new TLS certificate
    pub fn new(cert_chain: Vec<u8>, private_key: Vec<u8>) -> Self {
        Self {
            cert_chain,
            private_key,
        }
    }

    /// Load certificate and private key from PEM bytes
    pub fn from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<Self, io::Error> {
        // Validate certificate can be parsed
        let mut cert_reader = BufReader::new(cert_pem);
        certs(&mut cert_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Validate private key can be parsed
        let mut key_reader = BufReader::new(key_pem);
        private_key(&mut key_reader)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "No private key found"))?;

        Ok(Self {
            cert_chain: cert_pem.to_vec(),
            private_key: key_pem.to_vec(),
        })
    }

    /// Build rustls ServerConfig from this certificate
    pub fn to_server_config(&self) -> Result<Arc<ServerConfig>, io::Error> {
        // Parse certificate chain
        let mut cert_reader = BufReader::new(&self.cert_chain[..]);
        let certs = certs(&mut cert_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Parse private key
        let mut key_reader = BufReader::new(&self.private_key[..]);
        let key = private_key(&mut key_reader)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "No private key found"))?;

        // Build server config
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        Ok(Arc::new(config))
    }
}

/// SNI-based certificate resolver
///
/// Routes incoming TLS connections to the correct certificate based on SNI hostname.
#[allow(dead_code)] // Used in tests and future HTTPS server
#[derive(Default)]
pub struct SniResolver {
    /// Map of hostname -> TLS config
    configs: std::collections::HashMap<String, Arc<ServerConfig>>,
}

#[allow(dead_code)] // Used in tests and future HTTPS server
impl SniResolver {
    /// Create a new SNI resolver
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a certificate for a hostname
    pub fn add_cert(&mut self, hostname: String, cert: TlsCertificate) -> Result<(), io::Error> {
        let config = cert.to_server_config()?;
        self.configs.insert(hostname, config);
        Ok(())
    }

    /// Get TLS acceptor for a hostname (exact match only for now)
    pub fn get_acceptor(&self, hostname: &str) -> Option<TlsAcceptor> {
        self.configs
            .get(hostname)
            .map(|config| TlsAcceptor::from(Arc::clone(config)))
    }

    /// Check if a hostname has a certificate configured
    pub fn has_cert(&self, hostname: &str) -> bool {
        self.configs.contains_key(hostname)
    }
}

/// Load certificate from Kubernetes Secret
///
/// Expects Secret data with keys:
/// - `tls.crt`: Certificate chain (PEM format)
/// - `tls.key`: Private key (PEM format)
#[allow(dead_code)] // Used in gateway controller
pub async fn load_cert_from_secret(
    client: &kube::Client,
    namespace: &str,
    secret_name: &str,
) -> Result<TlsCertificate, io::Error> {
    use k8s_openapi::api::core::v1::Secret;
    use kube::api::Api;

    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = secrets
        .get(secret_name)
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;

    // Get certificate and key from Secret data
    let data = secret
        .data
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Secret has no data"))?;

    let cert_data = data
        .get("tls.crt")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Secret missing tls.crt"))?;

    let key_data = data
        .get("tls.key")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Secret missing tls.key"))?;

    TlsCertificate::from_pem(&cert_data.0, &key_data.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();

    /// Initialize crypto provider for tests
    fn init_crypto() {
        INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    /// RED: Test TLS certificate parsing from file
    #[test]
    fn test_tls_certificate_from_pem() {
        init_crypto();
        let cert_pem = include_bytes!("../../../test_fixtures/tls/example.com.crt");
        let key_pem = include_bytes!("../../../test_fixtures/tls/example.com.key");

        // Should parse successfully
        let cert =
            TlsCertificate::from_pem(cert_pem, key_pem).expect("Should parse valid certificate");

        assert_eq!(cert.cert_chain, cert_pem);
        assert_eq!(cert.private_key, key_pem);
    }

    /// RED: Test invalid certificate is rejected
    #[test]
    fn test_tls_certificate_invalid_pem() {
        let bad_cert = b"not a certificate";
        let bad_key = b"not a key";

        let result = TlsCertificate::from_pem(bad_cert, bad_key);
        assert!(result.is_err(), "Should reject invalid certificate");
    }

    /// RED: Test SNI resolver can store and retrieve certificates
    #[test]
    fn test_sni_resolver_add_cert() {
        init_crypto();
        let cert_pem = include_bytes!("../../../test_fixtures/tls/example.com.crt");
        let key_pem = include_bytes!("../../../test_fixtures/tls/example.com.key");

        let cert =
            TlsCertificate::from_pem(cert_pem, key_pem).expect("Should parse test certificate");

        let mut resolver = SniResolver::new();
        resolver
            .add_cert("example.com".to_string(), cert)
            .expect("Should add certificate");

        assert!(resolver.has_cert("example.com"));
        assert!(!resolver.has_cert("other.com"));
    }

    /// RED: Test SNI resolver returns acceptor for known hostname
    #[test]
    fn test_sni_resolver_get_acceptor() {
        init_crypto();
        let cert_pem = include_bytes!("../../../test_fixtures/tls/example.com.crt");
        let key_pem = include_bytes!("../../../test_fixtures/tls/example.com.key");

        let cert =
            TlsCertificate::from_pem(cert_pem, key_pem).expect("Should parse test certificate");

        let mut resolver = SniResolver::new();
        resolver
            .add_cert("example.com".to_string(), cert)
            .expect("Should add certificate");

        let acceptor = resolver.get_acceptor("example.com");
        assert!(
            acceptor.is_some(),
            "Should return acceptor for known hostname"
        );

        let no_acceptor = resolver.get_acceptor("unknown.com");
        assert!(
            no_acceptor.is_none(),
            "Should return None for unknown hostname"
        );
    }

    /// GREEN: Test K8s Secret loading function exists
    /// (Integration test will verify actual K8s interaction with real cluster)
    #[test]
    fn test_load_cert_from_secret_exists() {
        // This test verifies load_cert_from_secret() function exists and compiles
        // The function signature is:
        //   async fn load_cert_from_secret(client: &Client, namespace: &str, secret_name: &str) -> Result<TlsCertificate, io::Error>
        //
        // Actual K8s integration testing requires a real cluster and will be done
        // in end-to-end tests with kind cluster.
        //
        // The test passes if this function compiles (which it does)
    }
}
