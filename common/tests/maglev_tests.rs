/// Maglev Consistent Hashing Tests
///
/// Testing the Maglev algorithm implementation for backend selection.
/// Based on the Google Maglev paper: https://research.google/pubs/pub44824/
use common::{maglev_build_table, maglev_lookup, Backend, MAGLEV_TABLE_SIZE};

#[test]
fn test_maglev_table_size_is_prime() {
    // Maglev requires a prime number for the table size
    assert_eq!(MAGLEV_TABLE_SIZE, 65537);
    assert!(is_prime(MAGLEV_TABLE_SIZE));
}

#[test]
fn test_maglev_empty_backends() {
    let backends: Vec<Backend> = vec![];
    let table = maglev_build_table(&backends);

    // Empty backends should produce empty table (all None)
    assert_eq!(table.len(), MAGLEV_TABLE_SIZE);
    assert!(table.iter().all(|&idx| idx.is_none()));
}

#[test]
fn test_maglev_single_backend() {
    let backends = vec![
        Backend::new(0x0a000101, 8080, 100), // 10.0.1.1:8080
    ];

    let table = maglev_build_table(&backends);

    // Single backend should fill entire table
    assert_eq!(table.len(), MAGLEV_TABLE_SIZE);
    assert!(table.iter().all(|&idx| idx == Some(0)));
}

#[test]
fn test_maglev_two_backends_distribution() {
    let backends = vec![
        Backend::new(0x0a000101, 8080, 100), // 10.0.1.1:8080
        Backend::new(0x0a000102, 8080, 100), // 10.0.1.2:8080
    ];

    let table = maglev_build_table(&backends);

    // Count distribution
    let mut counts = vec![0usize; backends.len()];
    for idx in table.iter().filter_map(|&x| x) {
        counts[idx as usize] += 1;
    }

    // Each backend should get roughly 50% (within 5% variance)
    let expected_per_backend = MAGLEV_TABLE_SIZE / 2;
    for (i, count) in counts.iter().enumerate() {
        let percentage = (*count as f64) / (MAGLEV_TABLE_SIZE as f64);
        assert!(
            (percentage - 0.5).abs() < 0.05,
            "Backend {} has {:.2}% distribution (expected ~50%)",
            i,
            percentage * 100.0
        );

        // Within 5% of expected
        let diff = (*count as isize - expected_per_backend as isize).abs();
        let max_diff = (expected_per_backend as f64 * 0.05) as isize;
        assert!(
            diff < max_diff,
            "Backend {} has {} entries, expected ~{} (±{})",
            i,
            count,
            expected_per_backend,
            max_diff
        );
    }
}

#[test]
fn test_maglev_three_backends_distribution() {
    let backends = vec![
        Backend::new(0x0a000101, 8080, 100), // 10.0.1.1:8080
        Backend::new(0x0a000102, 8080, 100), // 10.0.1.2:8080
        Backend::new(0x0a000103, 8080, 100), // 10.0.1.3:8080
    ];

    let table = maglev_build_table(&backends);

    // Count distribution
    let mut counts = vec![0usize; backends.len()];
    for idx in table.iter().filter_map(|&x| x) {
        counts[idx as usize] += 1;
    }

    // Each backend should get roughly 33% (within 5% variance)
    for (i, count) in counts.iter().enumerate() {
        let percentage = (*count as f64) / (MAGLEV_TABLE_SIZE as f64);
        assert!(
            (percentage - 0.333).abs() < 0.05,
            "Backend {} has {:.2}% distribution (expected ~33.3%)",
            i,
            percentage * 100.0
        );
    }
}

#[test]
fn test_maglev_lookup_consistency() {
    let backends = vec![
        Backend::new(0x0a000101, 8080, 100),
        Backend::new(0x0a000102, 8080, 100),
        Backend::new(0x0a000103, 8080, 100),
    ];

    let table = maglev_build_table(&backends);

    // Same flow key should always return same backend
    let flow_key = 0x12345678u64;
    let backend1 = maglev_lookup(flow_key, &table);
    let backend2 = maglev_lookup(flow_key, &table);
    let backend3 = maglev_lookup(flow_key, &table);

    assert_eq!(backend1, backend2);
    assert_eq!(backend2, backend3);
}

#[test]
fn test_maglev_different_flows_different_backends() {
    let backends = vec![
        Backend::new(0x0a000101, 8080, 100),
        Backend::new(0x0a000102, 8080, 100),
        Backend::new(0x0a000103, 8080, 100),
    ];

    let table = maglev_build_table(&backends);

    // Different flow keys should distribute across backends
    let mut backend_hits = vec![0usize; backends.len()];

    for i in 0..10_000 {
        let flow_key = i as u64;
        if let Some(backend_idx) = maglev_lookup(flow_key, &table) {
            backend_hits[backend_idx as usize] += 1;
        }
    }

    // Each backend should get roughly 33% of flows
    for (i, hits) in backend_hits.iter().enumerate() {
        let percentage = (*hits as f64) / 10_000.0;
        assert!(
            (percentage - 0.333).abs() < 0.05,
            "Backend {} got {:.2}% of flows (expected ~33.3%)",
            i,
            percentage * 100.0
        );
    }
}

#[test]
fn test_maglev_minimal_disruption_on_backend_removal() {
    // Initial 3 backends
    let backends_before = vec![
        Backend::new(0x0a000101, 8080, 100),
        Backend::new(0x0a000102, 8080, 100),
        Backend::new(0x0a000103, 8080, 100),
    ];

    // Remove middle backend
    let backends_after = vec![
        Backend::new(0x0a000101, 8080, 100),
        Backend::new(0x0a000103, 8080, 100),
    ];

    let table_before = maglev_build_table(&backends_before);
    let table_after = maglev_build_table(&backends_after);

    // Count how many flows changed their backend assignment
    let mut changed = 0;
    let mut unchanged = 0;

    for i in 0..10_000 {
        let flow_key = i as u64;

        let before = maglev_lookup(flow_key, &table_before);
        let after = maglev_lookup(flow_key, &table_after);

        if let (Some(b), Some(a)) = (before, after) {
            // Map old backend indices to new ones
            // Backend 0 stays 0, backend 2 becomes 1
            let expected = if b == 0 {
                0 // Backend 0 unchanged
            } else if b == 2 {
                1 // Backend 2 -> index 1 in new list
            } else {
                continue; // Was backend 1, will definitely change
            };

            if a == expected {
                unchanged += 1;
            } else {
                changed += 1;
            }
        }
    }

    // With Maglev, disruption should be ~1/N
    // Removing 1 of 3 backends means ~33% disruption
    let disruption_rate = changed as f64 / (changed + unchanged) as f64;

    // Should be close to 1/3 (0.333) but allow some variance
    assert!(
        disruption_rate < 0.40,
        "Disruption rate {:.2}% is too high (expected <40%)",
        disruption_rate * 100.0
    );
}

#[test]
fn test_maglev_minimal_disruption_on_backend_addition() {
    // Initial 2 backends
    let backends_before = vec![
        Backend::new(0x0a000101, 8080, 100),
        Backend::new(0x0a000102, 8080, 100),
    ];

    // Add third backend
    let backends_after = vec![
        Backend::new(0x0a000101, 8080, 100),
        Backend::new(0x0a000102, 8080, 100),
        Backend::new(0x0a000103, 8080, 100),
    ];

    let table_before = maglev_build_table(&backends_before);
    let table_after = maglev_build_table(&backends_after);

    // Count flows that stayed on same backend
    let mut unchanged = 0;
    let mut total_checked = 0;

    for i in 0..10_000 {
        let flow_key = i as u64;

        let before = maglev_lookup(flow_key, &table_before);
        let after = maglev_lookup(flow_key, &table_after);

        if let (Some(b), Some(a)) = (before, after) {
            total_checked += 1;
            // Backend indices 0 and 1 stay the same
            if b == a && b < 2 {
                unchanged += 1;
            }
        }
    }

    // With Maglev, adding 1 backend means ~1/3 of flows move
    // So ~2/3 should stay unchanged
    let unchanged_rate = unchanged as f64 / total_checked as f64;

    assert!(
        unchanged_rate > 0.60,
        "Only {:.2}% of flows stayed on same backend (expected >60%)",
        unchanged_rate * 100.0
    );
}

#[test]
fn test_maglev_deterministic() {
    // Same backends should produce same table every time
    let backends = vec![
        Backend::new(0x0a000101, 8080, 100),
        Backend::new(0x0a000102, 8080, 100),
        Backend::new(0x0a000103, 8080, 100),
    ];

    let table1 = maglev_build_table(&backends);
    let table2 = maglev_build_table(&backends);

    assert_eq!(table1, table2, "Maglev table should be deterministic");
}

/// EMBEDDED COMPACT MAGLEV TESTS
/// Testing per-route embedded Maglev tables for multi-route scenarios

#[test]
fn test_embedded_compact_maglev_multi_route() {
    // Route 1: /api/users with backends [10.0.1.1, 10.0.1.2]
    let route1_backends = vec![
        Backend::new(0x0a000101, 8080, 100), // 10.0.1.1:8080
        Backend::new(0x0a000102, 8080, 100), // 10.0.1.2:8080
    ];

    // Route 2: /api/orders with backends [10.0.2.1, 10.0.2.2]
    let route2_backends = vec![
        Backend::new(0x0a000201, 9000, 100), // 10.0.2.1:9000
        Backend::new(0x0a000202, 9000, 100), // 10.0.2.2:9000
    ];

    // Build embedded tables for each route
    let table1 = common::maglev_build_compact_table(&route1_backends);
    let table2 = common::maglev_build_compact_table(&route2_backends);

    // Tables should be different (different backends)
    assert_ne!(
        table1.as_slice(),
        table2.as_slice(),
        "Different routes should have different Maglev tables"
    );

    // Each table should reference only its own backends (indices 0-1)
    assert!(
        table1.iter().all(|&idx| idx < 2),
        "Route 1 table should only reference backends 0-1"
    );
    assert!(
        table2.iter().all(|&idx| idx < 2),
        "Route 2 table should only reference backends 0-1"
    );

    // Test backend selection using embedded tables
    let flow_key = 0x12345678u64;

    let idx1 = common::maglev_lookup_compact(flow_key, &table1);
    let idx2 = common::maglev_lookup_compact(flow_key, &table2);

    // Both should select valid backend indices
    assert!(idx1 < 2, "Route 1 should select backend index 0 or 1");
    assert!(idx2 < 2, "Route 2 should select backend index 0 or 1");

    // Verify actual backends are different (from different pools)
    let backend1 = &route1_backends[idx1 as usize];
    let backend2 = &route2_backends[idx2 as usize];

    // Route 1 backends are in 10.0.1.0/24, Route 2 in 10.0.2.0/24
    assert!((backend1.ipv4 & 0xFFFFFF00) == 0x0a000100);
    assert!((backend2.ipv4 & 0xFFFFFF00) == 0x0a000200);
}

#[test]
fn test_embedded_compact_table_size() {
    // Compact table should use 4099 (prime) for good distribution
    use common::COMPACT_MAGLEV_SIZE;

    assert_eq!(COMPACT_MAGLEV_SIZE, 4099);
    assert!(is_prime(COMPACT_MAGLEV_SIZE));
}

#[test]
fn test_embedded_compact_table_fits_in_u8() {
    // With MAX_BACKENDS = 32, we can use u8 for backend indices
    // Note: MAX_BACKENDS (32) fits in u8 by design - const checked in common/src/lib.rs
    use common::MAX_BACKENDS;

    let backends: Vec<Backend> = (0..MAX_BACKENDS)
        .map(|i| Backend::new(0x0a000100 + i as u32, 8080, 100))
        .collect();

    let table = common::maglev_build_compact_table(&backends);

    // All indices should fit in u8 (< MAX_BACKENDS)
    assert!(
        table.iter().all(|&idx| idx < MAX_BACKENDS as u8),
        "All backend indices should fit in u8"
    );
}

#[test]
fn test_embedded_compact_distribution() {
    // Compact table (4099 entries) should have similar distribution to full table
    let backends = vec![
        Backend::new(0x0a000101, 8080, 100),
        Backend::new(0x0a000102, 8080, 100),
        Backend::new(0x0a000103, 8080, 100),
    ];

    let table = common::maglev_build_compact_table(&backends);

    // Count distribution
    let mut counts = vec![0usize; backends.len()];
    for &idx in table.iter() {
        counts[idx as usize] += 1;
    }

    // Each backend should get roughly 33% (within 5% variance)
    for (i, count) in counts.iter().enumerate() {
        let percentage = (*count as f64) / (common::COMPACT_MAGLEV_SIZE as f64);
        assert!(
            (percentage - 0.333).abs() < 0.05,
            "Backend {} has {:.2}% distribution (expected ~33.3%)",
            i,
            percentage * 100.0
        );
    }
}

// Helper function to check if number is prime
fn is_prime(n: usize) -> bool {
    if n <= 1 {
        return false;
    }
    if n <= 3 {
        return true;
    }
    if n.is_multiple_of(2) || n.is_multiple_of(3) {
        return false;
    }

    let mut i = 5;
    while i * i <= n {
        if n.is_multiple_of(i) || n.is_multiple_of(i + 2) {
            return false;
        }
        i += 6;
    }

    true
}
