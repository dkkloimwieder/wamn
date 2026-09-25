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

/// Every direct dependency of the engine, with the reason it is accepted. The
/// edge pays for each one, so a new dependency needs its own line here.
const ACCEPTED: &[(&str, &str)] = &[
    ("anyhow", "error context"),
    ("async-trait", "the host traits the engine declares"),
    (
        "boon",
        "route input schemas: a pure schema check with no I/O",
    ),
    ("hex", "the route CSRF digest"),
    (
        "hyper",
        "the expected-host router's request types, with no client or server feature",
    ),
    ("opentelemetry", "the route limiter's gauges"),
    ("serde", "wire and manifest types"),
    ("serde_json", "wire and manifest types"),
    ("sha2", "component and schema digests"),
    ("tokio", "timers, files and channels"),
    ("tracing", "spans and events"),
    (
        "wamn-catalog",
        "the release manifest and admitted components",
    ),
    ("wamn-component-policy", "component admission"),
    ("wamn-event-wire", "the causation a route delivery derives"),
    ("wamn-execution-contract", "canonical JSON"),
    (
        "wamn-project-state",
        "the platform component a registration delivery runs as",
    ),
    ("wamn-run-state", "the intent store trait"),
    ("wash-runtime", "the native runtime"),
    ("wasmparser", "component admission"),
    ("wasmtime", "the native runtime"),
    (
        "wasmtime-wasi-http",
        "the expected-host router's native HTTP types",
    ),
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
fn every_direct_dependency_is_accepted_with_a_reason() {
    let direct = normal_dependencies(&["--depth", "1"]);
    let direct: BTreeSet<&str> = direct
        .iter()
        .map(String::as_str)
        .filter(|name| *name != "wamn-engine")
        .collect();
    let accepted: BTreeSet<&str> = ACCEPTED.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        direct, accepted,
        "the engine's direct dependencies changed; give each new one a reason in ACCEPTED"
    );
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
