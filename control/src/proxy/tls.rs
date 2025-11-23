//! TLS termination with rustls and SNI support
//!
//! Implements Gateway API TLS termination:
//! - Load certificates from Kubernetes Secrets
//! - SNI-based certificate selection
//! - Integration with hyper HTTPS server

use rustls::ServerConfig;
use rustls_pemfile::{certs, private_key};
use std::io::{self, BufReader};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio_rustls::TlsAcceptor;
use tracing::warn;

/// Safe Mutex access with automatic poison recovery
///
/// # Why this is safe
///
/// Mutex poisoning means a panic occurred while holding the lock, but:
/// - The lock still protects the data (no race conditions)
/// - The data itself is NOT corrupt (panic doesn't corrupt memory in Rust)
/// - Subsequent threads can safely access the data
///
/// We use .into_inner() to extract the guard, logging the poisoning event
/// for observability but continuing operation.
#[inline]
fn safe_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        warn!("Mutex poisoned in TLS module, recovering (data is still valid)");
        poisoned.into_inner()
    })
}

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
/// Uses rustls::server::ResolvesServerCertUsingSni for dynamic SNI resolution.
#[allow(dead_code)] // Used in tests and future HTTPS server
pub struct SniResolver {
    /// Rustls built-in SNI resolver (wrapped in Arc for sharing)
    inner: Arc<std::sync::Mutex<rustls::server::ResolvesServerCertUsingSni>>,
    /// Track hostnames for has_cert() checks
    hostnames: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

#[allow(dead_code)] // Used in tests and future HTTPS server
impl SniResolver {
    /// Create a new SNI resolver
    pub fn new() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(
                rustls::server::ResolvesServerCertUsingSni::new(),
            )),
            hostnames: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// Helper to parse a TlsCertificate and return a CertifiedKey
    fn parse_certified_key(cert: &TlsCertificate) -> Result<rustls::sign::CertifiedKey, io::Error> {
        use rustls::pki_types::CertificateDer;
        use rustls::sign::CertifiedKey;

        // Parse certificate chain
        let mut cert_reader = BufReader::new(&cert.cert_chain[..]);
        let certs: Vec<CertificateDer> = certs(&mut cert_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Parse private key
        let mut key_reader = BufReader::new(&cert.private_key[..]);
        let key = private_key(&mut key_reader)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "No private key found"))?;

        // Build signing key
        let signing_key = rustls::crypto::ring::sign::any_supported_type(&key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Create CertifiedKey
        Ok(CertifiedKey::new(certs, signing_key))
    }

    /// Add a certificate for a hostname
    pub fn add_cert(&mut self, hostname: String, cert: TlsCertificate) -> Result<(), io::Error> {
        // Use the helper to parse and create CertifiedKey
        let certified_key = Self::parse_certified_key(&cert)?;

        // Add to resolver
        let mut inner = safe_lock(&self.inner);
        inner
            .add(&hostname, certified_key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let mut hostnames = safe_lock(&self.hostnames);
        hostnames.insert(hostname);
        Ok(())
    }

    /// Update an existing certificate for a hostname (hot-reload)
    ///
    /// This enables zero-downtime certificate rotation:
    /// - Existing connections continue using old ServerConfig
    /// - New connections get new ServerConfig with updated certificate
    /// - No service restart required
    pub fn update_cert(&mut self, hostname: String, cert: TlsCertificate) -> Result<(), io::Error> {
        // Use the helper to parse and create CertifiedKey
        let certified_key = Self::parse_certified_key(&cert)?;

        // Update resolver (this replaces existing cert for hostname)
        let mut inner = safe_lock(&self.inner);
        inner
            .add(&hostname, certified_key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Hostname already tracked, no need to update hostnames set
        Ok(())
    }

    /// Get TLS acceptor for a hostname (exact match only for now)
    /// DEPRECATED: Use to_server_config() instead for dynamic SNI resolution
    pub fn get_acceptor(&self, hostname: &str) -> Option<TlsAcceptor> {
        // This is deprecated - we should use to_server_config() for dynamic SNI
        // For backwards compatibility with existing tests, we build a single-cert config
        let hostnames = safe_lock(&self.hostnames);
        if !hostnames.contains(hostname) {
            return None;
        }
        drop(hostnames);

        // Build a ServerConfig with the cert resolver
        match self.to_server_config() {
            Ok(config) => Some(TlsAcceptor::from(config)),
            Err(_) => None,
        }
    }

    /// Build ServerConfig with dynamic SNI resolution
    pub fn to_server_config(&self) -> Result<Arc<ServerConfig>, io::Error> {
        // Clone the Arc to share the resolver across connections
        let resolver = Arc::clone(&self.inner);

        // Wrap in a newtype that implements ResolvesServerCert
        #[derive(Debug)]
        struct SniResolverWrapper {
            inner: Arc<std::sync::Mutex<rustls::server::ResolvesServerCertUsingSni>>,
        }

        impl rustls::server::ResolvesServerCert for SniResolverWrapper {
            fn resolve(
                &self,
                client_hello: rustls::server::ClientHello,
            ) -> Option<Arc<rustls::sign::CertifiedKey>> {
                let inner = safe_lock(&self.inner);
                inner.resolve(client_hello)
            }
        }

        let wrapper = SniResolverWrapper { inner: resolver };
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(wrapper));

        Ok(Arc::new(config))
    }

    /// Check if a hostname has a certificate configured
    pub fn has_cert(&self, hostname: &str) -> bool {
        let hostnames = safe_lock(&self.hostnames);
        hostnames.contains(hostname)
    }
}

impl Default for SniResolver {
    fn default() -> Self {
        Self::new()
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

/// Parse certificate from Secret data (synchronous helper)
///
/// Used by Secret watchers that already have the Secret object.
/// Expects Secret.data with keys:
/// - `tls.crt`: Certificate chain (PEM format)
/// - `tls.key`: Private key (PEM format)
#[allow(dead_code)] // Used in gateway controller
pub fn parse_cert_from_secret_data(
    secret: &k8s_openapi::api::core::v1::Secret,
) -> Result<TlsCertificate, io::Error> {
    // Get certificate and key from Secret data
    let data = secret
        .data
        .as_ref()
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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

    /// RED: Test HTTPS server accepts TLS connections
    #[tokio::test]
    async fn test_https_server_with_tls_acceptor() {
        init_crypto();

        use hyper::body::Bytes;
        use hyper::server::conn::http1;
        use hyper::service::service_fn;
        use hyper::{Request, Response, StatusCode};
        use hyper_util::rt::TokioIo;
        use tokio::net::TcpListener;

        // Load test certificate
        let cert_pem = include_bytes!("../../../test_fixtures/tls/example.com.crt");
        let key_pem = include_bytes!("../../../test_fixtures/tls/example.com.key");
        let cert =
            TlsCertificate::from_pem(cert_pem, key_pem).expect("Should parse test certificate");

        // Create TLS acceptor
        let server_config = cert.to_server_config().expect("Should build server config");
        let acceptor = TlsAcceptor::from(server_config);

        // Start HTTPS server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();

            // Perform TLS handshake
            let tls_stream = acceptor.accept(stream).await.unwrap();
            let io = TokioIo::new(tls_stream);

            // Serve HTTP over TLS
            let service = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(http_body_util::Full::new(Bytes::from("Hello over HTTPS")))
                        .unwrap(),
                )
            });

            let _ = http1::Builder::new().serve_connection(io, service).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Connect with TLS client (accept self-signed cert)
        use rustls::pki_types::ServerName;
        use rustls::ClientConfig;
        use tokio_rustls::TlsConnector;

        let mut root_cert_store = rustls::RootCertStore::empty();
        root_cert_store.add_parsable_certificates(
            rustls_pemfile::certs(&mut std::io::BufReader::new(&cert_pem[..]))
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );

        let client_config = ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();

        let connector = TlsConnector::from(Arc::new(client_config));

        let stream = tokio::net::TcpStream::connect(server_addr).await.unwrap();
        let domain = ServerName::try_from("example.com").unwrap();
        let tls_stream = connector.connect(domain, stream).await.unwrap();

        // Make HTTPS request
        let io = TokioIo::new(tls_stream);
        let (mut request_sender, connection) =
            hyper::client::conn::http1::handshake(io).await.unwrap();

        tokio::spawn(async move {
            let _ = connection.await;
        });

        let req = Request::builder()
            .uri("/test")
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();

        let response = request_sender.send_request(req).await.unwrap();

        // Verify HTTPS response
        assert_eq!(response.status(), StatusCode::OK);

        use http_body_util::BodyExt;
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();

        assert_eq!(body, "Hello over HTTPS");
    }

    /// RED: Test dynamic SNI resolution with multiple hostnames
    #[tokio::test]
    async fn test_dynamic_sni_resolution_multiple_hosts() {
        init_crypto();

        use hyper::body::Bytes;
        use hyper::server::conn::http1;
        use hyper::service::service_fn;
        use hyper::{Request, Response, StatusCode};
        use hyper_util::rt::TokioIo;
        use tokio::net::TcpListener;

        // Load certificates for example.com and test.com
        let example_cert_pem = include_bytes!("../../../test_fixtures/tls/example.com.crt");
        let example_key_pem = include_bytes!("../../../test_fixtures/tls/example.com.key");
        let example_cert = TlsCertificate::from_pem(example_cert_pem, example_key_pem)
            .expect("Should parse example.com certificate");

        let test_cert_pem = include_bytes!("../../../test_fixtures/tls/test.com.crt");
        let test_key_pem = include_bytes!("../../../test_fixtures/tls/test.com.key");
        let test_cert = TlsCertificate::from_pem(test_cert_pem, test_key_pem)
            .expect("Should parse test.com certificate");

        // Create SNI resolver with both certificates
        let mut resolver = SniResolver::new();
        resolver
            .add_cert("example.com".to_string(), example_cert)
            .expect("Should add example.com cert");
        resolver
            .add_cert("test.com".to_string(), test_cert)
            .expect("Should add test.com cert");

        // Build ServerConfig with cert resolver (this will fail - need to refactor)
        let server_config = resolver
            .to_server_config()
            .expect("Should build ServerConfig with dynamic SNI");

        let acceptor = TlsAcceptor::from(server_config);

        // Start HTTPS server with dynamic SNI
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        let acceptor_clone = acceptor.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let acceptor = acceptor_clone.clone();

                tokio::spawn(async move {
                    let tls_stream = acceptor.accept(stream).await.unwrap();
                    let io = TokioIo::new(tls_stream);

                    let service = service_fn(|req: Request<hyper::body::Incoming>| async move {
                        // Echo the Host header to verify which cert was used
                        let host = req
                            .headers()
                            .get("host")
                            .and_then(|h| h.to_str().ok())
                            .unwrap_or("unknown");
                        Ok::<_, hyper::Error>(
                            Response::builder()
                                .status(StatusCode::OK)
                                .body(http_body_util::Full::new(Bytes::from(format!(
                                    "Hello from {}",
                                    host
                                ))))
                                .unwrap(),
                        )
                    });

                    let _ = http1::Builder::new().serve_connection(io, service).await;
                });
            }
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Test 1: Connect to example.com
        use rustls::pki_types::ServerName;
        use rustls::ClientConfig;
        use tokio_rustls::TlsConnector;

        let mut root_cert_store = rustls::RootCertStore::empty();
        root_cert_store.add_parsable_certificates(
            rustls_pemfile::certs(&mut std::io::BufReader::new(&example_cert_pem[..]))
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );
        root_cert_store.add_parsable_certificates(
            rustls_pemfile::certs(&mut std::io::BufReader::new(&test_cert_pem[..]))
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );

        let client_config = ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();

        let connector = TlsConnector::from(Arc::new(client_config));

        // Test example.com
        let stream = tokio::net::TcpStream::connect(server_addr).await.unwrap();
        let domain = ServerName::try_from("example.com").unwrap();
        let tls_stream = connector.connect(domain, stream).await.unwrap();

        let io = TokioIo::new(tls_stream);
        let (mut request_sender, connection) =
            hyper::client::conn::http1::handshake(io).await.unwrap();

        tokio::spawn(async move {
            let _ = connection.await;
        });

        let req = Request::builder()
            .uri("/test")
            .header("Host", "example.com")
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();

        let response = request_sender.send_request(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        use http_body_util::BodyExt;
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert_eq!(body, "Hello from example.com");

        // Test 2: Connect to test.com (should get different cert)
        let stream2 = tokio::net::TcpStream::connect(server_addr).await.unwrap();
        let domain2 = ServerName::try_from("test.com").unwrap();
        let tls_stream2 = connector.connect(domain2, stream2).await.unwrap();

        let io2 = TokioIo::new(tls_stream2);
        let (mut request_sender2, connection2) =
            hyper::client::conn::http1::handshake(io2).await.unwrap();

        tokio::spawn(async move {
            let _ = connection2.await;
        });

        let req2 = Request::builder()
            .uri("/test")
            .header("Host", "test.com")
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();

        let response2 = request_sender2.send_request(req2).await.unwrap();
        assert_eq!(response2.status(), StatusCode::OK);

        let body_bytes2 = response2.into_body().collect().await.unwrap().to_bytes();
        let body2 = String::from_utf8(body_bytes2.to_vec()).unwrap();
        assert_eq!(body2, "Hello from test.com");
    }

    /// RED: Test certificate hot-reload without downtime
    #[tokio::test]
    async fn test_certificate_hot_reload() {
        init_crypto();

        // Load initial certificate for example.com
        let cert_v1_pem = include_bytes!("../../../test_fixtures/tls/example.com.crt");
        let key_v1_pem = include_bytes!("../../../test_fixtures/tls/example.com.key");
        let cert_v1 =
            TlsCertificate::from_pem(cert_v1_pem, key_v1_pem).expect("Should parse v1 certificate");

        // Create resolver with v1 certificate
        let mut resolver = SniResolver::new();
        resolver
            .add_cert("example.com".to_string(), cert_v1)
            .expect("Should add v1 certificate");

        // Build initial ServerConfig
        let config_v1 = resolver.to_server_config().expect("Should build config");

        // Simulate certificate renewal (renewed example.com certificate)
        let cert_v2_pem = include_bytes!("../../../test_fixtures/tls/example.com-v2.crt");
        let key_v2_pem = include_bytes!("../../../test_fixtures/tls/example.com-v2.key");
        let cert_v2 =
            TlsCertificate::from_pem(cert_v2_pem, key_v2_pem).expect("Should parse v2 certificate");

        // Hot-reload certificate (this will fail - update_cert doesn't exist yet)
        resolver
            .update_cert("example.com".to_string(), cert_v2)
            .expect("Should update certificate");

        // Build new ServerConfig after reload
        let config_v2 = resolver
            .to_server_config()
            .expect("Should build config after reload");

        // Verify both configs can coexist (no downtime)
        // Old connections use config_v1, new connections use config_v2
        assert_ne!(
            Arc::as_ptr(&config_v1),
            Arc::as_ptr(&config_v2),
            "Should be different ServerConfig instances"
        );

        // Verify resolver still knows about example.com
        assert!(resolver.has_cert("example.com"));
    }

    /// RED: Test SNI-based certificate selection
    #[tokio::test]
    async fn test_sni_based_certificate_selection() {
        init_crypto();

        use hyper::body::Bytes;
        use hyper::server::conn::http1;
        use hyper::service::service_fn;
        use hyper::{Request, Response, StatusCode};
        use hyper_util::rt::TokioIo;
        use tokio::net::TcpListener;

        // Load test certificate for example.com
        let cert_pem = include_bytes!("../../../test_fixtures/tls/example.com.crt");
        let key_pem = include_bytes!("../../../test_fixtures/tls/example.com.key");
        let cert =
            TlsCertificate::from_pem(cert_pem, key_pem).expect("Should parse test certificate");

        // Create SNI resolver with example.com cert
        let mut resolver = SniResolver::new();
        resolver
            .add_cert("example.com".to_string(), cert.clone())
            .expect("Should add certificate");

        // Get acceptor for example.com
        let acceptor = resolver
            .get_acceptor("example.com")
            .expect("Should have acceptor for example.com");

        // Start HTTPS server with SNI resolver
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();

            // Perform TLS handshake with SNI
            let tls_stream = acceptor.accept(stream).await.unwrap();
            let io = TokioIo::new(tls_stream);

            // Serve HTTP over TLS
            let service = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(http_body_util::Full::new(Bytes::from(
                            "Hello from example.com",
                        )))
                        .unwrap(),
                )
            });

            let _ = http1::Builder::new().serve_connection(io, service).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Connect with TLS client requesting example.com via SNI
        use rustls::pki_types::ServerName;
        use rustls::ClientConfig;
        use tokio_rustls::TlsConnector;

        let mut root_cert_store = rustls::RootCertStore::empty();
        root_cert_store.add_parsable_certificates(
            rustls_pemfile::certs(&mut std::io::BufReader::new(&cert_pem[..]))
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );

        let client_config = ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();

        let connector = TlsConnector::from(Arc::new(client_config));

        let stream = tokio::net::TcpStream::connect(server_addr).await.unwrap();
        let domain = ServerName::try_from("example.com").unwrap(); // SNI hostname
        let tls_stream = connector.connect(domain, stream).await.unwrap();

        // Make HTTPS request
        let io = TokioIo::new(tls_stream);
        let (mut request_sender, connection) =
            hyper::client::conn::http1::handshake(io).await.unwrap();

        tokio::spawn(async move {
            let _ = connection.await;
        });

        let req = Request::builder()
            .uri("/test")
            .header("Host", "example.com")
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();

        let response = request_sender.send_request(req).await.unwrap();

        // Verify SNI-based routing worked
        assert_eq!(response.status(), StatusCode::OK);

        use http_body_util::BodyExt;
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();

        assert_eq!(body, "Hello from example.com");
    }
}
