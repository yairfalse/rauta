// TDD RED Phase: Parser Contract Tests
//
// Goal: Document the contract that BPF must follow
// BPF parse_http_method() should use common::HttpMethod::from_bytes()
// and return (method, method.len())
//
// Why this matters:
// - BPF and common had duplicate logic (drift risk!)
// - Tests only validated common parser
// - BPF parser could behave differently
//
// TDD Cycle:
// 1. RED: This test documents expected behavior
// 2. GREEN: Refactor BPF to use common::HttpMethod::from_bytes()
// 3. REFACTOR: Remove duplicate code

use common::HttpMethod;

/// Contract: Parser returns method with correct length
/// This is what BPF parse_http_method() must implement
#[test]
fn test_parser_contract_method_with_length() {
    // Test all methods return correct (method, length) tuple
    let test_cases = vec![
        (b"GET /api HTTP/1.1" as &[u8], HttpMethod::GET, 3),
        (b"POST /api HTTP/1.1", HttpMethod::POST, 4),
        (b"PUT /api HTTP/1.1", HttpMethod::PUT, 3),
        (b"DELETE /api HTTP/1.1", HttpMethod::DELETE, 6),
        (b"HEAD /api HTTP/1.1", HttpMethod::HEAD, 4),
        (b"PATCH /api HTTP/1.1", HttpMethod::PATCH, 5),
        (b"OPTIONS /api HTTP/1.1", HttpMethod::OPTIONS, 7),
    ];

    for (input, expected_method, expected_len) in test_cases {
        // Parse method
        let method = HttpMethod::from_bytes(input)
            .unwrap_or_else(|| panic!("Should parse: {:?}", std::str::from_utf8(input)));

        // Verify method matches
        assert_eq!(method, expected_method);

        // Verify length matches
        assert_eq!(method.len() as usize, expected_len);

        // This is what BPF should do:
        // let (parsed_method, parsed_len) = parse_http_method(input)?;
        // assert_eq!(parsed_method, method);
        // assert_eq!(parsed_len, expected_len);
    }
}

/// Contract: Parser rejects invalid methods consistently
#[test]
fn test_parser_contract_rejects_invalid() {
    let invalid_inputs = vec![
        b"get /api" as &[u8], // lowercase
        b"GTE /api",          // typo
        b"123 /api",          // number
        b"GET/api",           // no space
        b"GE",                // too short
        b"",                  // empty
    ];

    for input in invalid_inputs {
        let result = HttpMethod::from_bytes(input);
        assert_eq!(
            result,
            None,
            "Should reject: {:?}",
            std::str::from_utf8(input).unwrap_or("<binary>")
        );
    }
}

/// Contract: Parser handles edge cases
#[test]
fn test_parser_contract_edge_cases() {
    // Minimum valid input: "GET " (4 bytes)
    assert_eq!(HttpMethod::from_bytes(b"GET "), Some(HttpMethod::GET));

    // Just at boundary
    assert_eq!(HttpMethod::from_bytes(b"GET"), None);

    // POST needs 5 bytes: "POST "
    assert_eq!(HttpMethod::from_bytes(b"POST "), Some(HttpMethod::POST));
    assert_eq!(HttpMethod::from_bytes(b"POST"), None);

    // Longer methods
    assert_eq!(HttpMethod::from_bytes(b"DELETE "), Some(HttpMethod::DELETE));
    assert_eq!(
        HttpMethod::from_bytes(b"OPTIONS "),
        Some(HttpMethod::OPTIONS)
    );
}

/// Contract: Method length must be accurate for path parsing
/// BPF uses this to skip "GET " and find "/api/users"
#[test]
fn test_parser_contract_length_for_path_extraction() {
    let request = b"GET /api/users HTTP/1.1";

    // Parse method
    let method = HttpMethod::from_bytes(request).unwrap();
    assert_eq!(method, HttpMethod::GET);

    // Get method length
    let method_len = method.len() as usize;
    assert_eq!(method_len, 3);

    // Skip "GET " (method + space) to find path
    let path_start = method_len + 1; // +1 for space
    let remaining = &request[path_start..];

    // Should start with "/"
    assert_eq!(remaining[0], b'/');

    // Extract path (up to space before HTTP/1.1)
    let path_end = remaining.iter().position(|&b| b == b' ').unwrap();
    let path = &remaining[..path_end];

    assert_eq!(path, b"/api/users");
}

/// Contract: Parser is const-evaluable (no runtime deps)
/// This allows BPF verifier to optimize it
#[test]
fn test_parser_contract_const_eval() {
    // from_bytes is const fn - can be evaluated at compile time
    const METHOD: Option<HttpMethod> = HttpMethod::from_bytes(b"GET /");
    assert_eq!(METHOD, Some(HttpMethod::GET));

    const LEN: u8 = HttpMethod::GET.len();
    assert_eq!(LEN, 3);
}

// ============================================================================
// RED Phase Complete
// ============================================================================
//
// These tests document the contract. Now:
//
// GREEN Phase: Refactor bpf/src/main.rs::parse_http_method() to:
//
//   fn parse_http_method(data: &[u8]) -> Option<(HttpMethod, usize)> {
//       let method = common::HttpMethod::from_bytes(data)?;
//       let method_len = method.len() as usize;
//       Some((method, method_len))
//   }
//
// REFACTOR Phase: Run all tests, verify integration tests pass
// ============================================================================
