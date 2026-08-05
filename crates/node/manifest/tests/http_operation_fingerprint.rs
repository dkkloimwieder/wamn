use wamn_node_manifest::{
    HTTP_OPERATION_FINGERPRINT_VERSION, HttpBodyDigest, HttpOperation,
    HttpOperationFingerprintErrorKind, HttpSemanticHeader, fingerprint_http_operation,
    is_http_operation_semantic_header, normalize_portable_http_target,
};

fn operation<'a>(
    method: &'a str,
    target: &'a str,
    headers: &'a [HttpSemanticHeader<'a>],
    body: &[u8],
) -> HttpOperation<'a> {
    HttpOperation {
        method,
        target: normalize_portable_http_target(target).expect("portable target spelling"),
        semantic_headers: headers,
        body_digest: HttpBodyDigest::sha256(body),
    }
}

fn fingerprint(operation: &HttpOperation<'_>) -> (Vec<u8>, [u8; 32]) {
    let fingerprint = fingerprint_http_operation(operation).expect("operation is canonicalizable");
    (
        fingerprint.canonical_bytes().to_vec(),
        *fingerprint.digest(),
    )
}

#[test]
fn equivalent_operations_have_identical_canonical_bytes_and_digest() {
    let first_headers = [
        HttpSemanticHeader {
            name: "Content-Type",
            value: " application/json\t",
        },
        HttpSemanticHeader {
            name: "X-Operation",
            value: "create",
        },
    ];
    let reordered_headers = [
        HttpSemanticHeader {
            name: "x-operation",
            value: "create",
        },
        HttpSemanticHeader {
            name: "content-type",
            value: "application/json",
        },
    ];
    let first = operation(
        "post",
        "/orders/%7eactive?customer=%41da&view=a%2fb",
        &first_headers,
        br#"{"sku":"A-1"}"#,
    );
    let equivalent = operation(
        "POST",
        "/orders/~active?customer=Ada&view=a%2Fb",
        &reordered_headers,
        br#"{"sku":"A-1"}"#,
    );

    assert_eq!(fingerprint(&first), fingerprint(&equivalent));
}

#[test]
fn every_semantic_field_mutation_changes_the_digest() {
    let headers = [HttpSemanticHeader {
        name: "content-type",
        value: "application/json",
    }];
    let changed_headers = [HttpSemanticHeader {
        name: "content-type",
        value: "text/plain",
    }];
    let baseline = fingerprint(&operation("POST", "/orders?mode=live", &headers, b"one")).1;

    for mutation in [
        operation("PUT", "/orders?mode=live", &headers, b"one"),
        operation("POST", "/orders?mode=dry", &headers, b"one"),
        operation("POST", "/orders?mode=live", &changed_headers, b"one"),
        operation("POST", "/orders?mode=live", &headers, b"two"),
    ] {
        assert_ne!(fingerprint(&mutation).1, baseline);
    }
}

#[test]
fn target_and_header_normalization_rules_are_deterministic() {
    let headers = [
        HttpSemanticHeader {
            name: "X-Zeta",
            value: "z",
        },
        HttpSemanticHeader {
            name: "x-alpha",
            value: " a ",
        },
    ];
    let canonical = fingerprint(&operation(
        "patch",
        "/v1/%75sers?separator=%2f&literal=%7E",
        &headers,
        b"",
    ))
    .0;
    let text = String::from_utf8_lossy(&canonical);

    assert!(text.contains("PATCH"));
    assert!(text.contains("v1/users?separator=%2F&literal=~"));
    let alpha = text.find("x-alpha").expect("alpha header is framed");
    let zeta = text.find("x-zeta").expect("zeta header is framed");
    assert!(
        alpha < zeta,
        "semantic headers are sorted by canonical name"
    );
}

#[test]
fn endpoint_and_environment_injections_fail_closed() {
    for target in [
        "https://prod.example/orders",
        "//prod.example/orders",
        "orders",
        "",
    ] {
        normalize_portable_http_target(target).expect_err("non-portable target spelling must fail");
    }

    for target in ["/../orders", "/orders/%2fadmin"] {
        let error = fingerprint_http_operation(&operation("POST", target, &[], b""))
            .expect_err("path escape must fail");
        assert_eq!(
            error.kind(),
            HttpOperationFingerprintErrorKind::InvalidRelativeTarget
        );
    }

    for name in [
        "Host",
        "Authorization",
        "Proxy-Authorization",
        "Idempotency-Key",
        "Content-Length",
    ] {
        let headers = [HttpSemanticHeader {
            name,
            value: "injected",
        }];
        let error = fingerprint_http_operation(&operation("POST", "/orders", &headers, b""))
            .expect_err("environment or system-owned header must fail");
        assert_eq!(
            error.kind(),
            HttpOperationFingerprintErrorKind::EnvironmentField
        );
    }
}

#[test]
fn canonical_preimage_omission_mutant_is_killed_by_named_frames() {
    let headers = [HttpSemanticHeader {
        name: "content-type",
        value: "application/json",
    }];
    let canonical = fingerprint(&operation("POST", "/orders", &headers, b"payload")).0;

    for frame in [
        HTTP_OPERATION_FINGERPRINT_VERSION.as_bytes(),
        b"method".as_slice(),
        b"relative-target".as_slice(),
        b"semantic-header-count".as_slice(),
        b"semantic-header-name".as_slice(),
        b"semantic-header-value".as_slice(),
        b"body-sha256".as_slice(),
    ] {
        assert!(
            canonical.windows(frame.len()).any(|window| window == frame),
            "canonical preimage omitted frame {:?}",
            String::from_utf8_lossy(frame)
        );
    }
}

#[test]
fn canonical_preimage_reordering_mutant_is_killed_by_golden_digest() {
    let headers = [
        HttpSemanticHeader {
            name: "x-operation",
            value: "create",
        },
        HttpSemanticHeader {
            name: "content-type",
            value: "application/json",
        },
    ];
    let digest = fingerprint(&operation(
        "POST",
        "/orders/%7Eactive?mode=live",
        &headers,
        br#"{"sku":"A-1"}"#,
    ))
    .1;

    assert_eq!(
        digest,
        [
            119, 231, 59, 21, 44, 254, 80, 98, 44, 232, 54, 184, 251, 169, 171, 187, 159, 83, 2,
            96, 112, 49, 95, 226, 166, 166, 38, 238, 73, 158, 108, 186,
        ],
        "update this pinned digest only for an intentional codec-version change"
    );
}

#[test]
fn duplicate_semantic_headers_fail_instead_of_using_ambient_joining_rules() {
    let headers = [
        HttpSemanticHeader {
            name: "X-Tag",
            value: "one",
        },
        HttpSemanticHeader {
            name: "x-tag",
            value: "two",
        },
    ];
    let error = fingerprint_http_operation(&operation("POST", "/orders", &headers, b""))
        .expect_err("duplicate semantic headers need contract-specific joining");
    assert_eq!(
        error.kind(),
        HttpOperationFingerprintErrorKind::InvalidSemanticHeader
    );
}

#[test]
fn tracing_metadata_is_not_part_of_operation_identity() {
    assert!(!is_http_operation_semantic_header("traceparent"));
    assert!(!is_http_operation_semantic_header("TraceParent"));
    assert!(is_http_operation_semantic_header("content-type"));
}
