//! Configuration for RAUTA controller
//!
//! Gateway API native configuration (no legacy Ingress support).

use serde::{Deserialize, Serialize};
use std::env;

/// Controller configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ControllerConfig {
    /// Controller name (for Gateway API)
    #[serde(default = "default_controller_name")]
    pub controller_name: String,

    /// GatewayClass name to watch (for Gateway API)
    pub gateway_class_name: Option<String>,

    /// Timeout configuration
    #[serde(default)]
    pub timeouts: TimeoutConfig,
}

/// Timeout configuration for reliability
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TimeoutConfig {
    /// TCP connection timeout in seconds (default: 5s)
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,

    /// HTTP request timeout in seconds (default: 30s)
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,

    /// Idle connection timeout in seconds (default: 90s)
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
}

fn default_connect_timeout() -> u64 {
    5
}

fn default_request_timeout() -> u64 {
    30
}

fn default_idle_timeout() -> u64 {
    90
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            connect_timeout_secs: default_connect_timeout(),
            request_timeout_secs: default_request_timeout(),
            idle_timeout_secs: default_idle_timeout(),
        }
    }
}

fn default_controller_name() -> String {
    "rauta.io/gateway-controller".to_string()
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            controller_name: default_controller_name(),
            gateway_class_name: Some("rauta".to_string()),
            timeouts: TimeoutConfig::default(),
        }
    }
}

impl ControllerConfig {
    /// Load configuration from environment variables (available but not yet integrated)
    #[allow(dead_code)]
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let mut config = Self::default();

        // Controller identity
        if let Ok(val) = env::var("RAUTA_CONTROLLER_NAME") {
            config.controller_name = val;
        }

        if let Ok(val) = env::var("RAUTA_GATEWAY_CLASS") {
            config.gateway_class_name = Some(val);
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ControllerConfig::default();
        assert_eq!(config.controller_name, "rauta.io/gateway-controller");
        assert_eq!(config.gateway_class_name, Some("rauta".to_string()));
    }

    #[test]
    fn test_timeout_defaults() {
        let config = ControllerConfig::default();

        // Production timeouts for reliability
        assert_eq!(
            config.timeouts.connect_timeout_secs, 5,
            "Connect timeout should be 5s (fast-fail for dead backends)"
        );
        assert_eq!(
            config.timeouts.request_timeout_secs, 30,
            "Request timeout should be 30s (reasonable for most requests)"
        );
        assert_eq!(
            config.timeouts.idle_timeout_secs, 90,
            "Idle timeout should be 90s (keep connections alive but not forever)"
        );
    }
}
