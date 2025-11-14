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

    /// Health checking configuration
    #[serde(default)]
    pub health_check: HealthCheckConfig,
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

/// Health check configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HealthCheckConfig {
    /// Enable active health checking (default: false for safety)
    #[serde(default = "default_false")]
    pub enabled: bool,

    /// Probe interval in seconds (default: 5s)
    #[serde(default = "default_health_check_interval")]
    pub interval_secs: u64,

    /// Probe timeout in seconds (default: 2s)
    #[serde(default = "default_health_check_timeout")]
    pub timeout_secs: u64,

    /// Consecutive failures before marking unhealthy (default: 3)
    #[serde(default = "default_unhealthy_threshold")]
    pub unhealthy_threshold: u32,

    /// Consecutive successes before marking healthy (default: 2)
    #[serde(default = "default_healthy_threshold")]
    pub healthy_threshold: u32,
}

fn default_false() -> bool {
    false
}

fn default_health_check_interval() -> u64 {
    5
}

fn default_health_check_timeout() -> u64 {
    2
}

fn default_unhealthy_threshold() -> u32 {
    3
}

fn default_healthy_threshold() -> u32 {
    2
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            enabled: default_false(), // Disabled by default for safety
            interval_secs: default_health_check_interval(),
            timeout_secs: default_health_check_timeout(),
            unhealthy_threshold: default_unhealthy_threshold(),
            healthy_threshold: default_healthy_threshold(),
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
            health_check: HealthCheckConfig::default(),
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

    #[test]
    fn test_health_check_defaults() {
        let config = ControllerConfig::default();

        // Health checking disabled by default for safety
        assert!(
            !config.health_check.enabled,
            "Health checking should be disabled by default"
        );
        assert_eq!(
            config.health_check.interval_secs, 5,
            "Health check interval should be 5s"
        );
        assert_eq!(
            config.health_check.timeout_secs, 2,
            "Health check timeout should be 2s"
        );
        assert_eq!(
            config.health_check.unhealthy_threshold, 3,
            "Unhealthy threshold should be 3"
        );
        assert_eq!(
            config.health_check.healthy_threshold, 2,
            "Healthy threshold should be 2"
        );
    }
}
