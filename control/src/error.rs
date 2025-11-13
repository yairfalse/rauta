use thiserror::Error;

/// RAUTA Control Plane Errors (reserved for future use)
#[allow(dead_code)]
#[derive(Error, Debug)]
pub enum RautaError {
    #[error("Route configuration error: {0}")]
    RouteConfig(String),

    #[error("Kubernetes error: {0}")]
    Kubernetes(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
