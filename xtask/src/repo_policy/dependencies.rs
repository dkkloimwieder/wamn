//! Normal dependency boundaries in both workspaces.

mod graph;

use anyhow::ensure;
use std::path::Path;

const DOMAINS: &[&str] = &[
    "wamn-application",
    "wamn-application-chart",
    "wamn-authentication",
    "wamn-authority",
    "wamn-blobstore",
    "wamn-component-policy",
    "wamn-connection",
    "wamn-connection-type",
    "wamn-dev",
    "wamn-digest",
    "wamn-edge-document",
    "wamn-envelope",
    "wamn-environment",
    "wamn-event",
    "wamn-identity-protocol",
    "wamn-number-pattern",
    "wamn-oci-reference",
    "wamn-package",
    "wamn-platform-install",
    "wamn-principal",
    "wamn-record-history",
    "wamn-registry",
    "wamn-release",
    "wamn-role",
    "wamn-routes",
    "wamn-run-state",
    "wamn-scalar",
    "wamn-schema",
    "wamn-workflow-graph",
];
const TECHNOLOGY: &[&str] = &[
    "async-nats",
    "hyper",
    "k8s-openapi",
    "kube",
    "object_store",
    "oci-client",
    "reqwest",
    "sqlx",
    "wash-runtime",
    "wasmtime",
];
// External service adapters. The edge uses the shared HTTP transport for forwarding.
const SERVICE_CLIENTS: &[&str] = &[
    "wamn-broker",
    "wamn-gcp-metadata",
    "wamn-http",
    "wamn-identity-client",
    "wamn-key",
    "wamn-kubernetes",
    "wamn-oci",
    "wamn-postgres",
    "wamn-provision",
    "wamn-storage",
];
const TEST_INFRASTRUCTURE: &str = "wamn-test-infrastructure";
const TEST_AND_TOOL_PACKAGES: &[&str] = &[
    TEST_INFRASTRUCTURE,
    "wamn-fixture-package",
    "wamn-grant-tests",
    "wamn-journey-tests",
    "wamn-receiving-tests",
    "wamn-wms-tests",
    "xtask",
    "connection-http-standard",
    "fixture",
    "fixture-overlay",
];

pub(crate) fn check_boundaries(root: &Path) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for manifest in ["Cargo.toml", "apps/Cargo.toml"] {
        graph::report(&root.join(manifest), refused, &mut failures)?;
    }
    failures.sort_unstable();
    failures.dedup();
    ensure!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

fn refused(source: &str, target: &str, runtime_exempt: bool) -> bool {
    let runtime = matches!(target, "wasmtime" | "wash-runtime") || target.starts_with("wasmtime-");
    (DOMAINS.contains(&source) && (TECHNOLOGY.contains(&target) || runtime))
        || (matches!(source, "wamn-control" | "wamn-identity-service")
            && runtime
            && !runtime_exempt)
        || (matches!(source, "wamn-edge" | "wamn-edge-document")
            && SERVICE_CLIENTS.contains(&target))
        || (target == TEST_INFRASTRUCTURE && !TEST_AND_TOOL_PACKAGES.contains(&source))
}
