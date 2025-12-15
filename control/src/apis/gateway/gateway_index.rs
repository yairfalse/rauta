//! Gateway Index - High-performance shared state for Gateway→GatewayClass mappings
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                         GatewayIndex                                     │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │  Data (Arc<RwLock<HashMap>>)        │  Metrics (Atomics - lock-free!)   │
//! │  ┌─────────────────────────────┐    │  ┌─────────────────────────────┐  │
//! │  │ (ns, name) → ()             │    │  │ lookups: AtomicU64          │  │
//! │  │ O(1) read, O(1) write       │    │  │ hits: AtomicU64             │  │
//! │  └─────────────────────────────┘    │  │ adds: AtomicU64             │  │
//! │                                      │  │ removes: AtomicU64          │  │
//! │                                      │  └─────────────────────────────┘  │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Performance
//!
//! - **Lookups**: O(1) HashMap + atomic counter increment (1 CPU instruction)
//! - **Metrics**: Lock-free atomics, only formatted on `/metrics` scrape
//! - **Thread-safe**: RwLock with poison recovery for data, Atomics for metrics

use crate::proxy::router::{safe_read, safe_write};
use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tracing::debug;

// =============================================================================
// GatewayKey
// =============================================================================

/// Key for Gateway lookup: (namespace, name)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GatewayKey {
    pub namespace: String,
    pub name: String,
}

impl GatewayKey {
    #[inline]
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
        }
    }
}

impl fmt::Display for GatewayKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.namespace, self.name)
    }
}

// =============================================================================
// GatewayIndexMetrics - Lock-free atomic counters
// =============================================================================

/// Lock-free metrics for GatewayIndex operations
///
/// Uses atomic operations for zero-cost tracking on the hot path.
/// Only formatted to strings when Prometheus scrapes `/metrics`.
#[derive(Debug, Default)]
pub struct GatewayIndexMetrics {
    /// Total lookup operations (contains() calls)
    lookups: AtomicU64,
    /// Successful lookups (Gateway was in index)
    hits: AtomicU64,
    /// Total add operations
    adds: AtomicU64,
    /// Total remove operations
    removes: AtomicU64,
}

impl GatewayIndexMetrics {
    /// Create new metrics (all counters at zero)
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a lookup operation
    #[inline(always)]
    pub fn record_lookup(&self, hit: bool) {
        // Relaxed ordering is fine for counters - we don't need synchronization
        self.lookups.fetch_add(1, Ordering::Relaxed);
        if hit {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record an add operation
    #[inline(always)]
    pub fn record_add(&self) {
        self.adds.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a remove operation
    #[inline(always)]
    pub fn record_remove(&self) {
        self.removes.fetch_add(1, Ordering::Relaxed);
    }

    /// Get current lookup count
    pub fn lookups(&self) -> u64 {
        self.lookups.load(Ordering::Relaxed)
    }

    /// Get current hit count
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Get current add count
    pub fn adds(&self) -> u64 {
        self.adds.load(Ordering::Relaxed)
    }

    /// Get current remove count
    pub fn removes(&self) -> u64 {
        self.removes.load(Ordering::Relaxed)
    }

    /// Calculate hit rate (0.0 - 1.0)
    pub fn hit_rate(&self) -> f64 {
        let lookups = self.lookups();
        if lookups == 0 {
            return 0.0;
        }
        self.hits() as f64 / lookups as f64
    }

    /// Format metrics in Prometheus exposition format
    ///
    /// Only called on `/metrics` scrape - not on hot path!
    pub fn to_prometheus(&self, gateway_class: &str) -> String {
        format!(
            r#"# HELP gateway_index_lookups_total Total Gateway index lookup operations
# TYPE gateway_index_lookups_total counter
gateway_index_lookups_total{{class="{class}"}} {lookups}
# HELP gateway_index_hits_total Successful Gateway index lookups
# TYPE gateway_index_hits_total counter
gateway_index_hits_total{{class="{class}"}} {hits}
# HELP gateway_index_adds_total Total Gateway index add operations
# TYPE gateway_index_adds_total counter
gateway_index_adds_total{{class="{class}"}} {adds}
# HELP gateway_index_removes_total Total Gateway index remove operations
# TYPE gateway_index_removes_total counter
gateway_index_removes_total{{class="{class}"}} {removes}
# HELP gateway_index_hit_rate Gateway index lookup hit rate
# TYPE gateway_index_hit_rate gauge
gateway_index_hit_rate{{class="{class}"}} {hit_rate}"#,
            class = gateway_class,
            lookups = self.lookups(),
            hits = self.hits(),
            adds = self.adds(),
            removes = self.removes(),
            hit_rate = self.hit_rate(),
        )
    }
}

// =============================================================================
// GatewayIndex
// =============================================================================

/// Thread-safe index of Gateways managed by our GatewayClass
///
/// ## Features
///
/// - **O(1) lookups** - HashSet with RwLock for concurrent access
/// - **Lock-free metrics** - Atomic counters, no overhead on hot path
/// - **Ergonomic API** - Display, Iterator, contains_any()
/// - **Poison recovery** - Continues operating after thread panic
///
/// ## Example
///
/// ```ignore
/// let index = GatewayIndex::new("rauta");
///
/// // Gateway controller: add/remove
/// index.add("default", "my-gateway");
/// index.remove("default", "old-gateway");
///
/// // HTTPRoute controller: O(1) lookup
/// if index.contains("default", "my-gateway") {
///     // Process route
/// }
///
/// // Prometheus scrape (cold path)
/// println!("{}", index.prometheus_metrics());
/// ```
#[derive(Debug)]
pub struct GatewayIndex {
    /// Set of managed Gateway keys: (namespace, name)
    inner: Arc<RwLock<HashSet<GatewayKey>>>,
    /// The GatewayClass name we're tracking
    gateway_class_name: String,
    /// Lock-free metrics (shared across clones)
    metrics: Arc<GatewayIndexMetrics>,
}

impl GatewayIndex {
    /// Create a new GatewayIndex for the specified GatewayClass
    pub fn new(gateway_class_name: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashSet::new())),
            gateway_class_name: gateway_class_name.into(),
            metrics: Arc::new(GatewayIndexMetrics::new()),
        }
    }

    /// Register a Gateway as managed by our GatewayClass
    ///
    /// Called by Gateway controller when a Gateway using our class is reconciled.
    /// Idempotent - safe to call multiple times.
    ///
    /// Returns `true` if the Gateway was newly added, `false` if already present.
    pub fn add(&self, namespace: &str, name: &str) -> bool {
        let key = GatewayKey::new(namespace, name);
        let mut inner = safe_write(&self.inner);
        let is_new = inner.insert(key);
        self.metrics.record_add();
        if is_new {
            debug!(
                "GatewayIndex: added {}/{} (total: {})",
                namespace,
                name,
                inner.len()
            );
        }
        is_new
    }

    /// Remove a Gateway from the index
    ///
    /// Called by Gateway controller when:
    /// - Gateway is deleted
    /// - Gateway changes to a different GatewayClass
    ///
    /// Returns `true` if the Gateway was present and removed, `false` otherwise.
    pub fn remove(&self, namespace: &str, name: &str) -> bool {
        let key = GatewayKey::new(namespace, name);
        let mut inner = safe_write(&self.inner);
        let was_present = inner.remove(&key);
        self.metrics.record_remove();
        if was_present {
            debug!(
                "GatewayIndex: removed {}/{} (total: {})",
                namespace,
                name,
                inner.len()
            );
        }
        was_present
    }

    /// Check if a Gateway is managed by our GatewayClass
    ///
    /// **O(1) lookup** - no API calls needed.
    /// Called by HTTPRoute controller to filter parentRefs.
    #[inline]
    pub fn contains(&self, namespace: &str, name: &str) -> bool {
        let key = GatewayKey::new(namespace, name);
        let inner = safe_read(&self.inner);
        let hit = inner.contains(&key);
        self.metrics.record_lookup(hit);
        hit
    }

    /// Check if any of the given Gateways are managed
    ///
    /// Useful for batch checking HTTPRoute parentRefs.
    pub fn contains_any<'a>(&self, refs: impl IntoIterator<Item = (&'a str, &'a str)>) -> bool {
        let inner = safe_read(&self.inner);
        for (namespace, name) in refs {
            let key = GatewayKey::new(namespace, name);
            let hit = inner.contains(&key);
            self.metrics.record_lookup(hit);
            if hit {
                return true;
            }
        }
        false
    }

    /// Get the GatewayClass name this index is tracking
    #[inline]
    pub fn gateway_class_name(&self) -> &str {
        &self.gateway_class_name
    }

    /// Get the number of managed Gateways
    pub fn len(&self) -> usize {
        let inner = safe_read(&self.inner);
        inner.len()
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get all managed Gateway keys
    ///
    /// Returns a snapshot - modifications during iteration won't be seen.
    pub fn keys(&self) -> Vec<GatewayKey> {
        let inner = safe_read(&self.inner);
        inner.iter().cloned().collect()
    }

    /// Iterate over managed Gateways
    ///
    /// Returns a snapshot - modifications during iteration won't be seen.
    pub fn iter(&self) -> impl Iterator<Item = GatewayKey> {
        self.keys().into_iter()
    }

    /// Get metrics reference (for testing/debugging)
    pub fn metrics(&self) -> &GatewayIndexMetrics {
        &self.metrics
    }

    /// Format metrics in Prometheus exposition format
    ///
    /// Call this on `/metrics` endpoint - NOT on hot path!
    pub fn prometheus_metrics(&self) -> String {
        let size = self.len();
        let base_metrics = self.metrics.to_prometheus(&self.gateway_class_name);
        format!(
            "{base_metrics}\n# HELP gateway_index_size Current number of managed Gateways\n# TYPE gateway_index_size gauge\ngateway_index_size{{class=\"{class}\"}} {size}",
            class = self.gateway_class_name,
        )
    }
}

// Implement Clone to share the same underlying data
impl Clone for GatewayIndex {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            gateway_class_name: self.gateway_class_name.clone(),
            metrics: Arc::clone(&self.metrics),
        }
    }
}

// Pretty printing for debugging
impl fmt::Display for GatewayIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys = self.keys();
        let count = keys.len();

        if count == 0 {
            write!(f, "GatewayIndex({}): empty", self.gateway_class_name)
        } else if count <= 3 {
            let names: Vec<_> = keys.iter().map(|k| k.to_string()).collect();
            write!(
                f,
                "GatewayIndex({}): {} gateway(s) [{}]",
                self.gateway_class_name,
                count,
                names.join(", ")
            )
        } else {
            let first_three: Vec<_> = keys.iter().take(3).map(|k| k.to_string()).collect();
            write!(
                f,
                "GatewayIndex({}): {} gateway(s) [{}, ...]",
                self.gateway_class_name,
                count,
                first_three.join(", ")
            )
        }
    }
}

// =============================================================================
// Global Metrics Access (for /metrics endpoint)
// =============================================================================

use std::sync::OnceLock;

/// Global GatewayIndex for metrics export
static GLOBAL_GATEWAY_INDEX: OnceLock<GatewayIndex> = OnceLock::new();

/// Register a GatewayIndex for global metrics access
///
/// Call this once during startup to enable /metrics export.
/// Safe to call multiple times - only the first call takes effect.
pub fn register_global_gateway_index(index: &GatewayIndex) {
    if GLOBAL_GATEWAY_INDEX.set(index.clone()).is_err() {
        debug!("GatewayIndex already registered globally");
    }
}

/// Get GatewayIndex metrics in Prometheus format
///
/// Returns empty string if no GatewayIndex has been registered.
/// Call this from the /metrics endpoint.
pub fn gateway_index_metrics() -> String {
    GLOBAL_GATEWAY_INDEX
        .get()
        .map(|index| index.prometheus_metrics())
        .unwrap_or_default()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // =========================================================================
    // Basic Operations
    // =========================================================================

    #[test]
    fn test_gateway_index_new() {
        let index = GatewayIndex::new("rauta");

        assert_eq!(index.gateway_class_name(), "rauta");
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_gateway_index_add_returns_is_new() {
        let index = GatewayIndex::new("rauta");

        // First add returns true (new)
        assert!(index.add("default", "my-gateway"));

        // Second add returns false (already exists)
        assert!(!index.add("default", "my-gateway"));

        // Different gateway returns true
        assert!(index.add("default", "other-gateway"));
    }

    #[test]
    fn test_gateway_index_remove_returns_was_present() {
        let index = GatewayIndex::new("rauta");

        // Remove nonexistent returns false
        assert!(!index.remove("default", "nonexistent"));

        // Add and remove returns true
        index.add("default", "my-gateway");
        assert!(index.remove("default", "my-gateway"));

        // Remove again returns false
        assert!(!index.remove("default", "my-gateway"));
    }

    #[test]
    fn test_gateway_index_contains() {
        let index = GatewayIndex::new("rauta");

        assert!(!index.contains("default", "my-gateway"));

        index.add("default", "my-gateway");
        assert!(index.contains("default", "my-gateway"));

        index.remove("default", "my-gateway");
        assert!(!index.contains("default", "my-gateway"));
    }

    #[test]
    fn test_gateway_index_contains_any() {
        let index = GatewayIndex::new("rauta");

        index.add("default", "gateway-1");
        index.add("prod", "gateway-2");

        // At least one matches
        assert!(index.contains_any([("default", "gateway-1"), ("other", "nope")]));

        // None match
        assert!(!index.contains_any([("other", "nope"), ("another", "missing")]));

        // Empty iterator
        assert!(!index.contains_any(std::iter::empty::<(&str, &str)>()));
    }

    #[test]
    fn test_gateway_index_namespace_isolation() {
        let index = GatewayIndex::new("rauta");

        index.add("namespace-a", "my-gateway");

        assert!(index.contains("namespace-a", "my-gateway"));
        assert!(!index.contains("namespace-b", "my-gateway"));
    }

    // =========================================================================
    // Metrics (Lock-free)
    // =========================================================================

    #[test]
    fn test_metrics_record_operations() {
        let index = GatewayIndex::new("rauta");

        // Initial state
        assert_eq!(index.metrics().lookups(), 0);
        assert_eq!(index.metrics().hits(), 0);
        assert_eq!(index.metrics().adds(), 0);
        assert_eq!(index.metrics().removes(), 0);

        // Add increments adds counter
        index.add("default", "gw1");
        assert_eq!(index.metrics().adds(), 1);

        // Contains increments lookups (miss)
        index.contains("default", "nonexistent");
        assert_eq!(index.metrics().lookups(), 1);
        assert_eq!(index.metrics().hits(), 0);

        // Contains increments lookups and hits (hit)
        index.contains("default", "gw1");
        assert_eq!(index.metrics().lookups(), 2);
        assert_eq!(index.metrics().hits(), 1);

        // Remove increments removes counter
        index.remove("default", "gw1");
        assert_eq!(index.metrics().removes(), 1);
    }

    #[test]
    fn test_metrics_hit_rate() {
        let index = GatewayIndex::new("rauta");

        // No lookups = 0% hit rate
        assert_eq!(index.metrics().hit_rate(), 0.0);

        index.add("default", "gw1");

        // 1 hit, 0 miss = 100%
        index.contains("default", "gw1");
        assert!((index.metrics().hit_rate() - 1.0).abs() < 0.001);

        // 1 hit, 1 miss = 50%
        index.contains("default", "nonexistent");
        assert!((index.metrics().hit_rate() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_metrics_prometheus_format() {
        let index = GatewayIndex::new("rauta");
        index.add("default", "gw1");
        index.contains("default", "gw1");

        let metrics = index.prometheus_metrics();

        assert!(metrics.contains("gateway_index_lookups_total{class=\"rauta\"}"));
        assert!(metrics.contains("gateway_index_hits_total{class=\"rauta\"}"));
        assert!(metrics.contains("gateway_index_adds_total{class=\"rauta\"}"));
        assert!(metrics.contains("gateway_index_size{class=\"rauta\"}"));
        assert!(metrics.contains("# TYPE gateway_index_lookups_total counter"));
        assert!(metrics.contains("# TYPE gateway_index_size gauge"));
    }

    #[test]
    fn test_metrics_shared_across_clones() {
        let index1 = GatewayIndex::new("rauta");
        let index2 = index1.clone();

        // Operations on index1 should be visible in index2's metrics
        index1.add("default", "gw1");
        index2.contains("default", "gw1");

        assert_eq!(index1.metrics().adds(), 1);
        assert_eq!(index2.metrics().adds(), 1); // Same underlying counter
        assert_eq!(index1.metrics().lookups(), 1);
        assert_eq!(index2.metrics().lookups(), 1);
    }

    // =========================================================================
    // Display and Iterator
    // =========================================================================

    #[test]
    fn test_display_empty() {
        let index = GatewayIndex::new("rauta");
        assert_eq!(format!("{}", index), "GatewayIndex(rauta): empty");
    }

    #[test]
    fn test_display_few_gateways() {
        let index = GatewayIndex::new("rauta");
        index.add("default", "gw1");
        index.add("prod", "gw2");

        let display = format!("{}", index);
        assert!(display.starts_with("GatewayIndex(rauta): 2 gateway(s)"));
        assert!(display.contains("default/gw1") || display.contains("prod/gw2"));
    }

    #[test]
    fn test_display_many_gateways_truncates() {
        let index = GatewayIndex::new("rauta");
        for i in 0..10 {
            index.add("default", &format!("gw{}", i));
        }

        let display = format!("{}", index);
        assert!(display.contains("10 gateway(s)"));
        assert!(display.contains("...")); // Truncated
    }

    #[test]
    fn test_iterator() {
        let index = GatewayIndex::new("rauta");
        index.add("default", "gw1");
        index.add("prod", "gw2");

        let keys: Vec<_> = index.iter().collect();
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn test_gateway_key_display() {
        let key = GatewayKey::new("default", "my-gateway");
        assert_eq!(format!("{}", key), "default/my-gateway");
    }

    // =========================================================================
    // Thread Safety
    // =========================================================================

    #[test]
    fn test_clone_shares_state() {
        let index1 = GatewayIndex::new("rauta");
        let index2 = index1.clone();

        index1.add("default", "gateway-1");
        assert!(index2.contains("default", "gateway-1"));
    }

    #[test]
    fn test_concurrent_access() {
        use std::thread;

        let index = GatewayIndex::new("rauta");

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let index = index.clone();
                thread::spawn(move || {
                    index.add("default", &format!("gateway-{}", i));
                    for j in 0..10 {
                        let _ = index.contains("default", &format!("gateway-{}", j));
                    }
                    index.remove("default", &format!("gateway-{}", i));
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("Thread should not panic");
        }

        assert!(index.is_empty());
        // Verify metrics were tracked
        assert!(index.metrics().lookups() > 0);
        assert!(index.metrics().adds() == 10);
        assert!(index.metrics().removes() == 10);
    }

    // =========================================================================
    // Integration with HTTPRoute parentRefs
    // =========================================================================

    #[test]
    fn test_filter_parent_refs_pattern() {
        let index = GatewayIndex::new("rauta");

        index.add("default", "rauta-gateway");
        index.add("production", "rauta-prod");

        // Simulate parentRefs from an HTTPRoute
        let parent_refs = vec![
            ("default", "rauta-gateway"), // Our Gateway ✓
            ("default", "nginx-gateway"), // Not ours ✗
            ("production", "rauta-prod"), // Our Gateway ✓
            ("other", "some-gateway"),    // Not ours ✗
        ];

        // Filter pattern used in HTTPRoute controller
        let matching: Vec<_> = parent_refs
            .into_iter()
            .filter(|(ns, name)| index.contains(ns, name))
            .collect();

        assert_eq!(matching.len(), 2);
        assert!(matching.contains(&("default", "rauta-gateway")));
        assert!(matching.contains(&("production", "rauta-prod")));
    }
}
