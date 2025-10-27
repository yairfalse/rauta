use anyhow::Result;
use aya::{
    include_bytes_aligned,
    maps::{HashMap, LruHashMap, PerCpuArray},
    Ebpf,
};
use common::{
    fnv1a_hash, Backend, BackendList, CompactMaglevTable, HttpMethod, Metrics, RouteKey,
};
use std::net::Ipv4Addr;

/// Helper to load BPF program for testing (doesn't attach to interface)
fn load_test_bpf() -> Result<Ebpf> {
    #[cfg(debug_assertions)]
    let bpf = Ebpf::load(include_bytes_aligned!(
        "../../target/bpfel-unknown-none/debug/rauta"
    ))?;

    #[cfg(not(debug_assertions))]
    let bpf = Ebpf::load(include_bytes_aligned!(
        "../../target/bpfel-unknown-none/release/rauta"
    ))?;

    Ok(bpf)
}

#[test]
#[ignore] // Requires BPF binary to be built first
fn test_single_route_single_backend() -> Result<()> {
    // Load BPF program (maps only, no XDP attachment)
    let mut bpf = load_test_bpf()?;

    // Create route: GET /api/users
    let path = b"/api/users";
    let path_hash = fnv1a_hash(path);
    let route_key = RouteKey::new(HttpMethod::GET, path_hash);

    // Backend: 10.0.1.1:8080
    let backend = Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100);

    let mut backend_list = BackendList::empty();
    backend_list.backends[0] = backend;
    backend_list.count = 1;

    // Build Maglev table
    let compact_table_vec = common::maglev_build_compact_table(&[backend]);
    let mut maglev_table = CompactMaglevTable::empty();
    maglev_table.table.copy_from_slice(&compact_table_vec);

    // Insert into ROUTES map
    {
        let mut routes: HashMap<_, RouteKey, BackendList> =
            HashMap::try_from(bpf.map_mut("ROUTES").expect("ROUTES map not found"))?;

        routes.insert(route_key, backend_list, 0)?;
    }

    // Insert into MAGLEV_TABLES map
    {
        let mut maglev_tables: HashMap<_, u64, CompactMaglevTable> = HashMap::try_from(
            bpf.map_mut("MAGLEV_TABLES")
                .expect("MAGLEV_TABLES map not found"),
        )?;

        maglev_tables.insert(path_hash, maglev_table, 0)?;
    }

    // Verify route was inserted
    {
        let routes: HashMap<_, RouteKey, BackendList> =
            HashMap::try_from(bpf.map("ROUTES").expect("ROUTES map not found"))?;

        let retrieved_list = routes.get(&route_key, 0)?.expect("Route not found");
        assert_eq!(retrieved_list.count, 1);
        assert_eq!(retrieved_list.backends[0].ip, backend.ip);
        assert_eq!(retrieved_list.backends[0].port, backend.port);
    }

    // Verify Maglev table was inserted
    {
        let maglev_tables: HashMap<_, u64, CompactMaglevTable> = HashMap::try_from(
            bpf.map("MAGLEV_TABLES")
                .expect("MAGLEV_TABLES map not found"),
        )?;

        let retrieved_table = maglev_tables
            .get(&path_hash, 0)?
            .expect("Maglev table not found");

        // All entries should point to backend 0 (only one backend)
        assert!(retrieved_table.table.iter().all(|&idx| idx == 0));
    }

    Ok(())
}

#[test]
#[ignore] // Requires BPF binary to be built first
fn test_multiple_routes_different_backends() -> Result<()> {
    let mut bpf = load_test_bpf()?;

    // Route 1: GET /api/users -> [10.0.1.1:8080, 10.0.1.2:8080]
    let path1 = b"/api/users";
    let path1_hash = fnv1a_hash(path1);
    let route1_key = RouteKey::new(HttpMethod::GET, path1_hash);

    let route1_backends = vec![
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100),
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 2)), 8080, 100),
    ];

    let mut backend_list1 = BackendList::empty();
    backend_list1.backends[0] = route1_backends[0];
    backend_list1.backends[1] = route1_backends[1];
    backend_list1.count = 2;

    // Route 2: POST /api/orders -> [10.0.2.1:9000, 10.0.2.2:9000, 10.0.2.3:9000]
    let path2 = b"/api/orders";
    let path2_hash = fnv1a_hash(path2);
    let route2_key = RouteKey::new(HttpMethod::POST, path2_hash);

    let route2_backends = vec![
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 2, 1)), 9000, 100),
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 2, 2)), 9000, 100),
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 2, 3)), 9000, 100),
    ];

    let mut backend_list2 = BackendList::empty();
    backend_list2.backends[0] = route2_backends[0];
    backend_list2.backends[1] = route2_backends[1];
    backend_list2.backends[2] = route2_backends[2];
    backend_list2.count = 3;

    // Build Maglev tables
    let table1_vec = common::maglev_build_compact_table(&route1_backends);
    let mut maglev_table1 = CompactMaglevTable::empty();
    maglev_table1.table.copy_from_slice(&table1_vec);

    let table2_vec = common::maglev_build_compact_table(&route2_backends);
    let mut maglev_table2 = CompactMaglevTable::empty();
    maglev_table2.table.copy_from_slice(&table2_vec);

    // Insert routes
    {
        let mut routes: HashMap<_, RouteKey, BackendList> =
            HashMap::try_from(bpf.map_mut("ROUTES").expect("ROUTES map not found"))?;

        routes.insert(route1_key, backend_list1, 0)?;
        routes.insert(route2_key, backend_list2, 0)?;
    }

    // Insert Maglev tables
    {
        let mut maglev_tables: HashMap<_, u64, CompactMaglevTable> = HashMap::try_from(
            bpf.map_mut("MAGLEV_TABLES")
                .expect("MAGLEV_TABLES map not found"),
        )?;

        maglev_tables.insert(path1_hash, maglev_table1, 0)?;
        maglev_tables.insert(path2_hash, maglev_table2, 0)?;
    }

    // Verify both routes exist
    {
        let routes: HashMap<_, RouteKey, BackendList> =
            HashMap::try_from(bpf.map("ROUTES").expect("ROUTES map not found"))?;

        let list1 = routes.get(&route1_key, 0)?.expect("Route 1 not found");
        assert_eq!(list1.count, 2);

        let list2 = routes.get(&route2_key, 0)?.expect("Route 2 not found");
        assert_eq!(list2.count, 3);
    }

    // Verify both Maglev tables exist and are different
    {
        let maglev_tables: HashMap<_, u64, CompactMaglevTable> = HashMap::try_from(
            bpf.map("MAGLEV_TABLES")
                .expect("MAGLEV_TABLES map not found"),
        )?;

        let table1 = maglev_tables
            .get(&path1_hash, 0)?
            .expect("Table 1 not found");
        let table2 = maglev_tables
            .get(&path2_hash, 0)?
            .expect("Table 2 not found");

        // Tables should be different (different backend sets)
        assert_ne!(table1.table, table2.table);

        // Table 1 should only reference backends 0-1
        assert!(table1.table.iter().all(|&idx| idx < 2));

        // Table 2 should only reference backends 0-2
        assert!(table2.table.iter().all(|&idx| idx < 3));
    }

    Ok(())
}

#[test]
#[ignore] // Requires BPF binary to be built first
fn test_maglev_distribution_quality() -> Result<()> {
    let mut bpf = load_test_bpf()?;

    // Route with 3 backends
    let path = b"/api/balanced";
    let path_hash = fnv1a_hash(path);
    let route_key = RouteKey::new(HttpMethod::GET, path_hash);

    let backends = vec![
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100),
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 2)), 8080, 100),
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 3)), 8080, 100),
    ];

    let mut backend_list = BackendList::empty();
    for (i, backend) in backends.iter().enumerate() {
        backend_list.backends[i] = *backend;
    }
    backend_list.count = 3;

    // Build Maglev table
    let table_vec = common::maglev_build_compact_table(&backends);
    let mut maglev_table = CompactMaglevTable::empty();
    maglev_table.table.copy_from_slice(&table_vec);

    // Insert into maps
    {
        let mut routes: HashMap<_, RouteKey, BackendList> =
            HashMap::try_from(bpf.map_mut("ROUTES").expect("ROUTES map not found"))?;
        routes.insert(route_key, backend_list, 0)?;
    }

    {
        let mut maglev_tables: HashMap<_, u64, CompactMaglevTable> = HashMap::try_from(
            bpf.map_mut("MAGLEV_TABLES")
                .expect("MAGLEV_TABLES map not found"),
        )?;
        maglev_tables.insert(path_hash, maglev_table, 0)?;
    }

    // Retrieve and test distribution
    {
        let maglev_tables: HashMap<_, u64, CompactMaglevTable> = HashMap::try_from(
            bpf.map("MAGLEV_TABLES")
                .expect("MAGLEV_TABLES map not found"),
        )?;

        let table = maglev_tables.get(&path_hash, 0)?.expect("Table not found");

        // Count distribution
        let mut counts = vec![0usize; 3];
        for &idx in table.table.iter() {
            counts[idx as usize] += 1;
        }

        // Each backend should get ~33% (within 5% variance)
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

    Ok(())
}

#[test]
#[ignore] // Requires BPF binary to be built first
fn test_flow_cache_structure() -> Result<()> {
    let mut bpf = load_test_bpf()?;

    // Get flow cache map
    let mut flow_cache: LruHashMap<_, u64, u32> =
        LruHashMap::try_from(bpf.map_mut("FLOW_CACHE").expect("FLOW_CACHE map not found"))?;

    // Insert test flow: (client_ip ^ path_hash) -> backend_idx
    let client_ip: u32 = u32::from(Ipv4Addr::new(192, 168, 1, 100));
    let path_hash: u64 = fnv1a_hash(b"/api/users");
    let flow_key: u64 = (client_ip as u64) ^ path_hash;
    let backend_idx: u32 = 1;

    flow_cache.insert(flow_key, backend_idx, 0)?;

    // Verify insertion
    let retrieved_idx = flow_cache.get(&flow_key, 0)?.expect("Flow not found");
    assert_eq!(retrieved_idx, backend_idx);

    Ok(())
}

#[test]
#[ignore] // Requires BPF binary to be built first
fn test_metrics_structure() -> Result<()> {
    let mut bpf = load_test_bpf()?;

    // Get metrics map
    let metrics: PerCpuArray<_, Metrics> =
        PerCpuArray::try_from(bpf.map_mut("METRICS").expect("METRICS map not found"))?;

    // Get initial metrics (index 0)
    let per_cpu_values = metrics.get(&0, 0)?;

    // Should have metrics for each CPU
    assert!(per_cpu_values.iter().count() > 0);

    // Each CPU should have initialized metrics
    for cpu_metrics in per_cpu_values.iter() {
        // Initial values should be 0
        assert_eq!(cpu_metrics.packets_total, 0);
        assert_eq!(cpu_metrics.packets_tier1, 0);
        assert_eq!(cpu_metrics.packets_tier2, 0);
        assert_eq!(cpu_metrics.packets_tier3, 0);
        assert_eq!(cpu_metrics.packets_dropped, 0);
        assert_eq!(cpu_metrics.http_parse_errors, 0);
    }

    Ok(())
}

#[test]
#[ignore] // Requires BPF binary to be built first
fn test_route_key_uniqueness() -> Result<()> {
    let mut bpf = load_test_bpf()?;

    // Same path, different methods -> different routes
    let path = b"/api/users";
    let path_hash = fnv1a_hash(path);

    let get_key = RouteKey::new(HttpMethod::GET, path_hash);
    let post_key = RouteKey::new(HttpMethod::POST, path_hash);
    let put_key = RouteKey::new(HttpMethod::PUT, path_hash);

    // Create different backend lists
    let backend1 = Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100);
    let backend2 = Backend::new(u32::from(Ipv4Addr::new(10, 0, 2, 1)), 9000, 100);
    let backend3 = Backend::new(u32::from(Ipv4Addr::new(10, 0, 3, 1)), 7000, 100);

    let mut list1 = BackendList::empty();
    list1.backends[0] = backend1;
    list1.count = 1;

    let mut list2 = BackendList::empty();
    list2.backends[0] = backend2;
    list2.count = 1;

    let mut list3 = BackendList::empty();
    list3.backends[0] = backend3;
    list3.count = 1;

    // Insert all three routes
    {
        let mut routes: HashMap<_, RouteKey, BackendList> =
            HashMap::try_from(bpf.map_mut("ROUTES").expect("ROUTES map not found"))?;

        routes.insert(get_key, list1, 0)?;
        routes.insert(post_key, list2, 0)?;
        routes.insert(put_key, list3, 0)?;
    }

    // Verify all three routes are separate
    {
        let routes: HashMap<_, RouteKey, BackendList> =
            HashMap::try_from(bpf.map("ROUTES").expect("ROUTES map not found"))?;

        let retrieved1 = routes.get(&get_key, 0)?.expect("GET route not found");
        let retrieved2 = routes.get(&post_key, 0)?.expect("POST route not found");
        let retrieved3 = routes.get(&put_key, 0)?.expect("PUT route not found");

        assert_eq!(retrieved1.backends[0].ip, backend1.ip);
        assert_eq!(retrieved2.backends[0].ip, backend2.ip);
        assert_eq!(retrieved3.backends[0].ip, backend3.ip);
    }

    Ok(())
}
