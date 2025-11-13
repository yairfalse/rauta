use thiserror::Error;

/// RAUTA Control Plane Errors (reserved for future use)
#[allow(dead_code)]
#[derive(Error, Debug)]
pub enum RautaError {
    #[error("Route configuration error: {0}")]
    RouteConfigError(String),

    #[error("Kubernetes error: {0}")]
    KubernetesError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}
