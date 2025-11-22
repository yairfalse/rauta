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

/// Generate test TLS certificate (self-signed)
pub fn generate_test_cert() -> (Vec<u8>, Vec<u8>) {
    // For testing, we'll use a dummy cert/key
    // In production tests, generate proper self-signed cert with openssl
    let cert = b"-----BEGIN CERTIFICATE-----
MIICljCCAX4CCQCKz8Vz8vZ8ZDANBgkqhkiG9w0BAQsFADANMQswCQYDVQQGEwJV
UzAeFw0yNTAxMDEwMDAwMDBaFw0yNjAxMDEwMDAwMDBaMA0xCzAJBgNVBAYTAlVT
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA0Z8Vz8vZ8ZDANBgkqhki
-----END CERTIFICATE-----"
        .to_vec();

    let key = b"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDRnxXPy9nxkMA0
-----END PRIVATE KEY-----"
        .to_vec();

    (cert, key)
}
