use common::{fnv1a_hash, HttpMethod};

/// Test HTTP method parsing from byte slices
#[test]
fn test_http_method_from_bytes_get() {
    assert_eq!(HttpMethod::from_bytes(b"GET /"), Some(HttpMethod::GET));
    assert_eq!(HttpMethod::from_bytes(b"GET "), Some(HttpMethod::GET));
}

#[test]
fn test_http_method_from_bytes_post() {
    assert_eq!(HttpMethod::from_bytes(b"POST /"), Some(HttpMethod::POST));
    assert_eq!(HttpMethod::from_bytes(b"POST "), Some(HttpMethod::POST));
}

#[test]
fn test_http_method_from_bytes_all_methods() {
    assert_eq!(HttpMethod::from_bytes(b"GET /"), Some(HttpMethod::GET));
    assert_eq!(HttpMethod::from_bytes(b"POST /"), Some(HttpMethod::POST));
    assert_eq!(HttpMethod::from_bytes(b"PUT /"), Some(HttpMethod::PUT));
    assert_eq!(
        HttpMethod::from_bytes(b"DELETE /"),
        Some(HttpMethod::DELETE)
    );
    assert_eq!(HttpMethod::from_bytes(b"HEAD /"), Some(HttpMethod::HEAD));
    assert_eq!(HttpMethod::from_bytes(b"PATCH /"), Some(HttpMethod::PATCH));
    assert_eq!(
        HttpMethod::from_bytes(b"OPTIONS /"),
        Some(HttpMethod::OPTIONS)
    );
}

#[test]
fn test_http_method_from_bytes_invalid() {
    // Too short
    assert_eq!(HttpMethod::from_bytes(b"GE"), None);
    assert_eq!(HttpMethod::from_bytes(b""), None);

    // Invalid method
    assert_eq!(HttpMethod::from_bytes(b"INVALID"), None);
    assert_eq!(HttpMethod::from_bytes(b"TRACE /"), None);

    // Missing space
    assert_eq!(HttpMethod::from_bytes(b"GET"), None);
}

#[test]
fn test_http_method_length() {
    assert_eq!(HttpMethod::GET.len(), 3);
    assert_eq!(HttpMethod::PUT.len(), 3);
    assert_eq!(HttpMethod::POST.len(), 4);
    assert_eq!(HttpMethod::HEAD.len(), 4);
    assert_eq!(HttpMethod::PATCH.len(), 5);
    assert_eq!(HttpMethod::DELETE.len(), 6);
    assert_eq!(HttpMethod::OPTIONS.len(), 7);
}

/// Test FNV-1a hashing consistency
#[test]
fn test_fnv1a_hash_consistency() {
    let path1 = b"/api/users";
    let path2 = b"/api/users";
    let path3 = b"/api/posts";

    // Same input produces same hash
    assert_eq!(fnv1a_hash(path1), fnv1a_hash(path2));

    // Different inputs produce different hashes
    assert_ne!(fnv1a_hash(path1), fnv1a_hash(path3));
}

#[test]
fn test_fnv1a_hash_common_paths() {
    // These should all produce different hashes
    let hashes: Vec<u64> = vec![
        fnv1a_hash(b"/"),
        fnv1a_hash(b"/api"),
        fnv1a_hash(b"/api/users"),
        fnv1a_hash(b"/api/users/123"),
        fnv1a_hash(b"/health"),
        fnv1a_hash(b"/metrics"),
    ];

    // All hashes should be unique
    for (i, &hash1) in hashes.iter().enumerate() {
        for (j, &hash2) in hashes.iter().enumerate() {
            if i != j {
                assert_ne!(hash1, hash2, "Collision between hash {} and {}", i, j);
            }
        }
    }
}

#[test]
fn test_fnv1a_hash_empty_input() {
    let hash_empty = fnv1a_hash(b"");
    // Should return FNV offset basis
    assert_eq!(hash_empty, 0xcbf29ce484222325);
}

#[test]
fn test_fnv1a_hash_long_paths() {
    // Test with realistic long paths
    let long_path =
        b"/api/v1/namespaces/default/pods/my-pod-12345/logs?follow=true&timestamps=true";
    let hash = fnv1a_hash(long_path);

    // Should not panic and should produce a hash
    assert_ne!(hash, 0);

    // Consistent for same input
    assert_eq!(hash, fnv1a_hash(long_path));
}

#[test]
fn test_fnv1a_hash_similar_paths() {
    // Very similar paths should produce different hashes
    let hash1 = fnv1a_hash(b"/api/user");
    let hash2 = fnv1a_hash(b"/api/users");
    let hash3 = fnv1a_hash(b"/api/user/");

    assert_ne!(hash1, hash2);
    assert_ne!(hash1, hash3);
    assert_ne!(hash2, hash3);
}

/// Test real HTTP request lines
#[test]
fn test_real_http_request_lines() {
    // Simulate what we'd see in actual packets
    let requests: Vec<&[u8]> = vec![
        b"GET /api/users HTTP/1.1\r\n",
        b"POST /api/auth/login HTTP/1.1\r\n",
        b"PUT /api/users/123 HTTP/1.1\r\n",
        b"DELETE /api/sessions/abc HTTP/1.1\r\n",
    ];

    for request in &requests {
        let method = HttpMethod::from_bytes(request);
        assert!(
            method.is_some(),
            "Failed to parse: {:?}",
            std::str::from_utf8(request)
        );
    }
}

#[test]
fn test_path_extraction_simulation() {
    // Simulate extracting path from request line
    let request = b"GET /api/users?limit=10 HTTP/1.1\r\n";

    // Parse method
    let method = HttpMethod::from_bytes(request).unwrap();
    assert_eq!(method, HttpMethod::GET);

    // Extract path (would be done in BPF)
    let path_start = method.len() as usize + 1; // Skip "GET "
    let path_end = request[path_start..]
        .iter()
        .position(|&b| b == b' ')
        .unwrap();
    let path = &request[path_start..path_start + path_end];

    // Compute hash
    let hash = fnv1a_hash(path);
    assert_ne!(hash, 0);

    // Should be consistent
    assert_eq!(hash, fnv1a_hash(b"/api/users?limit=10"));
}
