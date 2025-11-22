//! Reusable K8s resource fixtures

/// Generate a Gateway YAML with HTTPS listener
pub fn gateway_with_https(
    name: &str,
    namespace: &str,
    cert_name: &str,
) -> String {
    format!(
        r#"
apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: {name}
  namespace: {namespace}
spec:
  gatewayClassName: rauta
  listeners:
  - name: https
    port: 443
    protocol: HTTPS
    tls:
      mode: Terminate
      certificateRefs:
      - name: {cert_name}
"#,
        name = name,
        namespace = namespace,
        cert_name = cert_name
    )
}

/// Generate a Gateway YAML with HTTP listener
pub fn gateway_with_http(name: &str, namespace: &str) -> String {
    format!(
        r#"
apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: {name}
  namespace: {namespace}
spec:
  gatewayClassName: rauta
  listeners:
  - name: http
    port: 80
    protocol: HTTP
"#,
        name = name,
        namespace = namespace
    )
}

/// Generate an HTTPRoute YAML
pub fn http_route(
    name: &str,
    namespace: &str,
    gateway_name: &str,
    path: &str,
    backend_name: &str,
    backend_port: u16,
) -> String {
    format!(
        r#"
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: {name}
  namespace: {namespace}
spec:
  parentRefs:
  - name: {gateway_name}
  rules:
  - matches:
    - path:
        type: PathPrefix
        value: {path}
    backendRefs:
    - name: {backend_name}
      port: {backend_port}
"#,
        name = name,
        namespace = namespace,
        gateway_name = gateway_name,
        path = path,
        backend_name = backend_name,
        backend_port = backend_port
    )
}

/// Generate a test backend Deployment + Service
pub fn test_backend(name: &str, namespace: &str, port: u16, replicas: u32) -> String {
    format!(
        r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {name}
  namespace: {namespace}
spec:
  replicas: {replicas}
  selector:
    matchLabels:
      app: {name}
  template:
    metadata:
      labels:
        app: {name}
    spec:
      containers:
      - name: backend
        image: hashicorp/http-echo:latest
        args:
        - "-text=Hello from {name}"
        - "-listen=:{port}"
        ports:
        - containerPort: {port}
---
apiVersion: v1
kind: Service
metadata:
  name: {name}
  namespace: {namespace}
spec:
  selector:
    app: {name}
  ports:
  - port: {port}
    targetPort: {port}
"#,
        name = name,
        namespace = namespace,
        port = port,
        replicas = replicas
    )
}

/// Generate test TLS certificate (self-signed) at runtime
///
/// Uses rcgen to create a proper self-signed certificate for testing.
/// This avoids hardcoding private keys in the repository (which triggers security scanners).
pub fn generate_test_cert() -> (Vec<u8>, Vec<u8>) {
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};

    // Generate a new key pair
    let key_pair = KeyPair::generate().expect("Failed to generate key pair");

    // Configure certificate parameters
    let mut params = CertificateParams::new(vec![
        "example.com".to_string(),
        "*.example.com".to_string(),
    ]).expect("Failed to create certificate params");

    // Set distinguished name
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CountryName, "US");
    dn.push(DnType::OrganizationName, "RAUTA Test");
    dn.push(DnType::CommonName, "example.com");
    params.distinguished_name = dn;

    // Set validity period (1 year)
    params.not_before = time::OffsetDateTime::now_utc();
    params.not_after = params.not_before + time::Duration::days(365);

    // Generate self-signed certificate
    let cert = params
        .self_signed(&key_pair)
        .expect("Failed to generate self-signed certificate");

    // Serialize to PEM format
    let cert_pem = cert.pem().into_bytes();
    let key_pem = key_pair.serialize_pem().into_bytes();

    (cert_pem, key_pem)
}
