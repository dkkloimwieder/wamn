//! The crates wamn-engine refuses to link, read from `cargo tree`.
//!
//! `cargo metadata` resolves features for the whole workspace, so a metadata
//! walk from this crate reports `oci-client` that only wamn-runtime turns on.
//! `cargo tree -p wamn-engine` resolves this crate alone.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Every crate the engine refuses, with the rule that refuses it.
const REFUSED: &[(&str, &str)] = &[
    ("tokio-postgres", "the engine links no Postgres"),
    ("deadpool-postgres", "the engine links no Postgres"),
    ("postgres-types", "the engine links no Postgres"),
    ("async-nats", "the engine links no network service"),
    ("object_store", "the engine links no network service"),
    ("redis", "the engine links no network service"),
    ("oci-client", "the engine links no OCI registry client"),
    ("oci-wasm", "the engine links no OCI registry client"),
    ("hyper-util", "the engine links no HTTP client"),
    ("hyper-rustls", "the engine links no HTTP client"),
    ("reqwest", "the engine links no HTTP client"),
    (
        "opentelemetry-otlp",
        "the engine links no OTLP gRPC exporter",
    ),
    ("tonic", "the engine links no OTLP gRPC exporter"),
    (
        "wamn-router",
        "routes-router rule 7: the base platform and the edge do not link the router",
    ),
    (
        "wamn-runtime",
        "the plugin set sits above the engine and the edge never links it",
    ),
];

/// The refused crates wash-runtime links with every feature off. The owner
/// accepted this list (finding `wamn-qt1t`).
const WASH_RUNTIME_LIST: &[&str] = &[
    "async-nats",
    "hyper-rustls",
    "hyper-util",
    "opentelemetry-otlp",
    "redis",
    "tonic",
];

/// The crate names in `cargo tree -p wamn-engine -e normal`, plus `extra` flags.
fn normal_dependencies(extra: &[&str]) -> BTreeSet<String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("wamn-engine lives three levels below the workspace root");
    let output = Command::new(env!("CARGO"))
        .current_dir(workspace)
        .args(["tree", "-p", "wamn-engine", "-e", "normal"])
        .args(extra)
        .args(["--prefix", "none", "--locked", "--offline"])
        .output()
        .expect("run cargo tree");
    assert!(
        output.status.success(),
        "cargo tree failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("cargo tree prints UTF-8")
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_engine_links_no_refused_crate_outside_wash_runtime() {
    let linked = normal_dependencies(&["--prune", "wash-runtime"]);
    let violations: Vec<String> = REFUSED
        .iter()
        .filter(|(name, _)| linked.contains(*name))
        .map(|(name, rule)| format!("wamn-engine links `{name}`: {rule}"))
        .collect();
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn wash_runtime_brings_exactly_the_accepted_refused_crates() {
    let linked = normal_dependencies(&[]);
    let present: BTreeSet<&str> = REFUSED
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| linked.contains(*name))
        .collect();
    let accepted: BTreeSet<&str> = WASH_RUNTIME_LIST.iter().copied().collect();
    assert_eq!(
        present, accepted,
        "the refused crates wamn-engine links through wash-runtime changed; the accepted \
         list (finding wamn-qt1t) is {accepted:?}"
    );
}
