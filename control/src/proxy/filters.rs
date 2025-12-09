//! Gateway API HTTPRoute Filters
//!
//! Implements filters for request/response transformation:
//! - RequestHeaderModifier: Modify request headers before proxying
//! - ResponseHeaderModifier: Modify response headers after proxying
//! - RequestRedirect: Redirect requests (status codes 301, 302)
//! - Timeout: Request and backend timeout configuration (Extended feature)

use std::time::Duration;

/// HTTP redirect status code (Gateway API HTTPRequestRedirectFilter)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)] // Used in tests during TDD implementation
pub enum RedirectStatusCode {
    /// 301 Moved Permanently
    MovedPermanently = 301,
    /// 302 Found (default)
    #[default]
    Found = 302,
}

/// Request redirect filter (Gateway API HTTPRequestRedirectFilter - Core feature)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(dead_code)] // Used in tests during TDD implementation
pub struct RequestRedirect {
    /// HTTP status code (301 or 302)
    pub status_code: RedirectStatusCode,
    /// Scheme to redirect to (e.g., "https")
    pub scheme: Option<String>,
    /// Hostname to redirect to (e.g., "new.example.com")
    pub hostname: Option<String>,
    /// Port to redirect to (e.g., 443)
    pub port: Option<u16>,
    /// Path to redirect to (replaces entire path)
    pub path: Option<String>,
}

impl RequestRedirect {
    /// Create a new redirect filter with default status code (302)
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn new() -> Self {
        Self::default()
    }

    /// Set status code (301 or 302)
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn status_code(mut self, code: RedirectStatusCode) -> Self {
        self.status_code = code;
        self
    }

    /// Set scheme (e.g., "https")
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn scheme(mut self, scheme: String) -> Self {
        self.scheme = Some(scheme);
        self
    }

    /// Set hostname (e.g., "new.example.com")
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn hostname(mut self, hostname: String) -> Self {
        self.hostname = Some(hostname);
        self
    }

    /// Set port (e.g., 443)
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Set path (replaces entire path)
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn path(mut self, path: String) -> Self {
        self.path = Some(path);
        self
    }
}

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

/// Request timeout configuration (Gateway API HTTPRouteTimeouts - Extended feature)
///
/// Specifies timeouts for the entire request and for backend requests.
/// Gateway API spec: https://gateway-api.sigs.k8s.io/reference/spec/#gateway.networking.k8s.io/v1.HTTPRouteTimeouts
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(dead_code)] // Used in tests during TDD implementation
pub struct Timeout {
    /// Overall request timeout (includes all retries, queuing, etc.)
    /// If None, no request-level timeout is enforced
    pub request: Option<Duration>,

    /// Backend request timeout (time allowed for a single backend attempt)
    /// If None, no backend-level timeout is enforced
    pub backend_request: Option<Duration>,
}

impl Timeout {
    /// Create a new empty timeout configuration
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn new() -> Self {
        Self::default()
    }

    /// Set overall request timeout
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn request(mut self, timeout: Duration) -> Self {
        self.request = Some(timeout);
        self
    }

    /// Set backend request timeout
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn backend_request(mut self, timeout: Duration) -> Self {
        self.backend_request = Some(timeout);
        self
    }
}

/// Retry configuration (Gateway API HTTPRouteRetry - Extended feature)
///
/// Specifies retry behavior for failed requests.
/// Gateway API spec: https://gateway-api.sigs.k8s.io/reference/spec/
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Used in tests during TDD implementation
pub struct RetryConfig {
    /// Maximum number of retry attempts (default: 3)
    pub max_retries: u32,

    /// Base delay for exponential backoff (default: 25ms)
    /// Actual delay = base_delay * 2^attempt (with jitter)
    pub base_delay: Duration,

    /// Maximum delay between retries (default: 1s)
    pub max_delay: Duration,

    /// Retry on 5xx status codes (default: true)
    pub retry_on_5xx: bool,

    /// Retry on connection errors (default: true)
    pub retry_on_connection_error: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(25),
            max_delay: Duration::from_secs(1),
            retry_on_5xx: true,
            retry_on_connection_error: true,
        }
    }
}

impl RetryConfig {
    /// Create a new retry configuration with defaults
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn new() -> Self {
        Self::default()
    }

    /// Set maximum retry attempts
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn max_retries(mut self, count: u32) -> Self {
        self.max_retries = count;
        self
    }

    /// Set base delay for exponential backoff
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn base_delay(mut self, delay: Duration) -> Self {
        self.base_delay = delay;
        self
    }

    /// Set maximum delay between retries
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = delay;
        self
    }

    /// Enable/disable retry on 5xx status codes
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn retry_on_5xx(mut self, enabled: bool) -> Self {
        self.retry_on_5xx = enabled;
        self
    }

    /// Enable/disable retry on connection errors
    #[allow(dead_code)] // Used in tests during TDD implementation
    pub fn retry_on_connection_error(mut self, enabled: bool) -> Self {
        self.retry_on_connection_error = enabled;
        self
    }

    /// Calculate delay for a given attempt using exponential backoff
    #[allow(dead_code)] // Used in retry implementation
    pub fn calculate_delay(&self, attempt: u32) -> Duration {
        // Exponential backoff: base_delay * 2^attempt
        let delay_ms = self.base_delay.as_millis() as u64 * (1u64 << attempt.min(10));
        Duration::from_millis(delay_ms).min(self.max_delay)
    }

    /// Check if a status code should trigger a retry
    #[allow(dead_code)] // Used in retry implementation
    pub fn should_retry_status(&self, status_code: u16) -> bool {
        self.retry_on_5xx && (500..600).contains(&status_code)
    }
}
