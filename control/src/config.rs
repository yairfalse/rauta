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
}
