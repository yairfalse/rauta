//! RAUTA Control Plane Library
//!
//! Exposes proxy, router, and backend pool modules for examples and tests

pub mod admin;
pub mod observability;
pub mod proxy;

// Re-export commonly used modules
mod apis;
mod config;
mod error;
