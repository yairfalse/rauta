//! Gateway API HTTPRoute Filters
//!
//! Implements filters for request/response transformation:
//! - RequestHeaderModifier: Modify request headers before proxying
//! - ResponseHeaderModifier: Modify response headers after proxying
//! - RequestRedirect: Redirect requests (status codes 301, 302)

/// Request header modification operation
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Used in tests during TDD implementation
pub enum HeaderModifierOp {
    /// Set header (add or replace)
    Set { name: String, value: String },
    /// Add header (multiple values allowed)
    Add { name: String, value: String },
    /// Remove header
    Remove { name: String },
}

/// Request header modifier filter (Gateway API HTTPRouteBackendRequestHeaderModification)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(dead_code)] // Used in tests during TDD implementation
pub struct RequestHeaderModifier {
    /// List of header operations to apply
    pub operations: Vec<HeaderModifierOp>,
}

impl RequestHeaderModifier {
    /// Create a new empty request header modifier
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn new() -> Self {
        Self {
            operations: Vec::new(),
        }
    }

    /// Add a "set" operation (add or replace header)
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn set(mut self, name: String, value: String) -> Self {
        self.operations.push(HeaderModifierOp::Set { name, value });
        self
    }

    /// Add an "add" operation (append header, allows multiple values)
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn add(mut self, name: String, value: String) -> Self {
        self.operations.push(HeaderModifierOp::Add { name, value });
        self
    }

    /// Add a "remove" operation
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn remove(mut self, name: String) -> Self {
        self.operations.push(HeaderModifierOp::Remove { name });
        self
    }
}

/// Response header modifier filter (Gateway API HTTPResponseHeaderModifier)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(dead_code)] // Used in tests during TDD implementation
pub struct ResponseHeaderModifier {
    /// List of header operations to apply to response
    pub operations: Vec<HeaderModifierOp>,
}

impl ResponseHeaderModifier {
    /// Create a new empty response header modifier
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn new() -> Self {
        Self {
            operations: Vec::new(),
        }
    }

    /// Add a "set" operation (add or replace header)
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn set(mut self, name: String, value: String) -> Self {
        self.operations.push(HeaderModifierOp::Set { name, value });
        self
    }

    /// Add an "add" operation (append header, allows multiple values)
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn add(mut self, name: String, value: String) -> Self {
        self.operations.push(HeaderModifierOp::Add { name, value });
        self
    }

    /// Add a "remove" operation
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn remove(mut self, name: String) -> Self {
        self.operations.push(HeaderModifierOp::Remove { name });
        self
    }
}
