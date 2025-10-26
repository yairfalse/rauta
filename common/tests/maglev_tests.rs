/// Maglev Consistent Hashing Tests
///
/// Testing the Maglev algorithm implementation for backend selection.
/// Based on the Google Maglev paper: https://research.google/pubs/pub44824/

use common::{Backend, maglev_build_table, maglev_lookup, MAGLEV_TABLE_SIZE};

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
            i, count, expected_per_backend, max_diff
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

        match (before, after) {
            (Some(b), Some(a)) => {
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
            _ => {}
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

// Helper function to check if number is prime
fn is_prime(n: usize) -> bool {
    if n <= 1 {
        return false;
    }
    if n <= 3 {
        return true;
    }
    if n % 2 == 0 || n % 3 == 0 {
        return false;
    }

    let mut i = 5;
    while i * i <= n {
        if n % i == 0 || n % (i + 2) == 0 {
            return false;
        }
        i += 6;
    }

    true
}
