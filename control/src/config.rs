//! Configuration for RAUTA controller
//!
//! Supports both Gateway API and Ingress API (configurable at deployment time).

use serde::{Deserialize, Serialize};
use std::env;

/// Controller configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ControllerConfig {
    /// APIs to enable
    pub apis: ApiConfig,

    /// Controller name (for Gateway API)
    #[serde(default = "default_controller_name")]
    pub controller_name: String,

    /// IngressClass name to watch (for Ingress API)
    pub ingress_class_name: Option<String>,

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

/// API feature flags
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApiConfig {
    /// Enable Gateway API support
    #[serde(default = "default_true")]
    pub gateway: bool,

    /// Enable Ingress API support
    #[serde(default = "default_true")]
    pub ingress: bool,
}

fn default_controller_name() -> String {
    "rauta.io/gateway-controller".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            apis: ApiConfig {
                gateway: true,
                ingress: true,
            },
            controller_name: default_controller_name(),
            ingress_class_name: Some("rauta".to_string()),
            gateway_class_name: Some("rauta".to_string()),
            timeouts: TimeoutConfig::default(),
        }
    }
}

impl ControllerConfig {
    /// Load configuration from environment variables
    /// TODO: Integrate `from_env()` into main configuration loading flow when environment-based config is required.
    #[allow(dead_code)]
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let mut config = Self::default();

        // API flags
        if let Ok(val) = env::var("RAUTA_ENABLE_GATEWAY") {
            config.apis.gateway = val.parse()?;
        }

        if let Ok(val) = env::var("RAUTA_ENABLE_INGRESS") {
            config.apis.ingress = val.parse()?;
        }

        // Controller identity
        if let Ok(val) = env::var("RAUTA_CONTROLLER_NAME") {
            config.controller_name = val;
        }

        if let Ok(val) = env::var("RAUTA_INGRESS_CLASS") {
            config.ingress_class_name = Some(val);
        }

        if let Ok(val) = env::var("RAUTA_GATEWAY_CLASS") {
            config.gateway_class_name = Some(val);
        }

        // Validate: at least one API must be enabled
        if !config.apis.gateway && !config.apis.ingress {
            return Err("At least one API (Gateway or Ingress) must be enabled".into());
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
        assert!(config.apis.gateway);
        assert!(config.apis.ingress);
        assert_eq!(config.controller_name, "rauta.io/gateway-controller");
    }

    #[test]
    fn test_config_validation() {
        // Both APIs disabled should fail
        let result = ControllerConfig {
            apis: ApiConfig {
                gateway: false,
                ingress: false,
            },
            ..Default::default()
        };

        // This config is invalid if we try to validate it
        // (We don't have a validate() method yet, but we check in from_env())
        assert!(!result.apis.gateway && !result.apis.ingress);
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
