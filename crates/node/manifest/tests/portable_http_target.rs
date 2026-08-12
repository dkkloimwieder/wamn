use wamn_node_manifest::normalize_portable_http_target;

#[test]
fn portable_http_target_has_one_connection_relative_spelling() {
    let target =
        normalize_portable_http_target("/orders?state=open").expect("portable target spelling");
    assert_eq!(target.as_str(), "orders?state=open");
}

#[test]
fn authority_and_ambiguous_target_spellings_fail_closed() {
    for target in [
        "https://prod.example/orders",
        "//prod.example/orders",
        "orders",
        "/orders#fragment",
        "/orders\\admin",
        "",
        "/",
    ] {
        normalize_portable_http_target(target).expect_err("non-portable target spelling must fail");
    }
}
