//! 5.5c integration: the dependency allowlist over a REAL fixture crate whose
//! transitive closure introduces an off-policy crate (`hex`). Drives the actual
//! `cargo metadata` path (offline) + the pure check, so the refusal is proven
//! end-to-end, not just over a synthetic name list.

use std::path::PathBuf;

use wamn_builder::allowlist::{AllowlistError, Policy, check_allowlist, resolved_package_names};

#[tokio::test]
async fn fixture_with_off_allowlist_dep_is_refused_by_name() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/disallowed-dep-node/Cargo.toml");

    let resolved = resolved_package_names(&manifest, "disallowed-dep-node")
        .await
        .expect("cargo metadata resolves the fixture offline");
    assert!(
        resolved.contains(&"hex".to_string()),
        "the fixture closure must carry hex: {resolved:?}"
    );

    let policy = Policy::default_policy();
    match check_allowlist(&resolved, &policy, "disallowed-dep-node") {
        Err(AllowlistError::DisallowedDependencies { package, denied }) => {
            assert_eq!(package, "disallowed-dep-node");
            assert!(
                denied.contains(&"hex".to_string()),
                "the refusal must name hex: {denied:?}"
            );
        }
        Ok(()) => panic!("the off-allowlist fixture dependency was ADMITTED — the gate is open"),
    }
}
