use thiserror::Error;

/// RAUTA Control Plane Errors (Stage 2+: eBPF errors)
#[allow(dead_code)]
#[derive(Error, Debug)]
pub enum RautaError {
    #[error("BPF load error: {0}")]
    BpfLoadError(String),

    #[error("Map not found: {0}")]
    MapNotFound(String),

    #[error("Map access error: {0}")]
    MapAccessError(String),

    #[error("Route configuration error: {0}")]
    RouteConfigError(String),

    #[error("Kubernetes error: {0}")]
    KubernetesError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}
