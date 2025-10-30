//! Kubernetes API integrations
//!
//! This module contains watchers and handlers for different Kubernetes APIs:
//! - Gateway API (v1): Modern, expressive routing API
//! - Ingress API (v1): Legacy compatibility support

pub mod gateway;
pub mod ingress;
pub mod metrics;
