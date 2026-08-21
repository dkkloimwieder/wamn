// SPDX-License-Identifier: Apache-2.0

const CLAIMS_SOURCE: &str =
    include_str!("../../../crates/platform/runtime/src/plugins/wamn_postgres/claims.rs");
const POOL_SOURCE: &str =
    include_str!("../../../crates/platform/runtime/src/plugins/wamn_postgres/pool.rs");
const FLOWBENCH_SOURCE: &str = include_str!("../../integration/src/flowbench.rs");
const MATBENCH_SOURCE: &str = include_str!("../../integration/src/matbench.rs");

#[test]
fn production_database_url_claims_stay_retired() {
    let (claims_production, claims_tests) = CLAIMS_SOURCE
        .split_once("#[cfg(test)]\nmod tests {")
        .expect("claims.rs must keep one explicit test module boundary");

    for (path, source) in [
        (
            "wamn_postgres/claims.rs production source",
            claims_production,
        ),
        ("flowbench.rs", FLOWBENCH_SOURCE),
        ("matbench.rs", MATBENCH_SOURCE),
    ] {
        assert!(
            !source.contains("DATABASE_URL"),
            "{path} advertises the retired DATABASE_URL source",
        );
    }

    assert!(
        claims_tests.contains("std::env::var(\"DATABASE_URL\")"),
        "the test-only disposable-PostgreSQL fallback is a separate contract",
    );
    assert!(
        POOL_SOURCE.contains("database_url: std::env::var(\"WAMN_PG_URL\").ok()"),
        "the production configuration must keep one explicit WAMN_PG_URL source",
    );
    assert!(
        !POOL_SOURCE.contains("DATABASE_URL"),
        "the production configuration restored the retired DATABASE_URL source",
    );
}
