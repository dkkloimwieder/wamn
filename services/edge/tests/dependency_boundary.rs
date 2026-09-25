//! The crates wamn-edge refuses to link, read from `cargo tree` for the build
//! host and for the box.
//!
//! `cargo tree -p wamn-edge` resolves this crate alone. `--target` resolves the
//! dependencies of the aarch64 box, which can differ from the build host.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Every crate the edge refuses, with the rule that refuses it.
const REFUSED: &[(&str, &str)] = &[
    ("wamn-runtime", "the edge never links the cloud plugin set"),
    ("wamn-workflow", "the edge links no workflow layer"),
    (
        "wamn-router",
        "routes-router rule 7: the base platform and the edge do not link the router",
    ),
    (
        "wamn-execution-host",
        "the cloud execution host depends on wamn-runtime",
    ),
    (
        "wamn-platform-identity",
        "it links tokio-postgres; the edge verifies sessions through wamn-session",
    ),
    ("tokio-postgres", "the edge links no Postgres"),
    ("deadpool-postgres", "the edge links no Postgres"),
    ("postgres-types", "the edge links no Postgres"),
    ("async-nats", "the edge links no NATS"),
    ("object_store", "the edge links no network service"),
    ("redis", "the edge links no network service"),
    ("oci-client", "the edge links no OCI registry client"),
    ("oci-wasm", "the edge links no OCI registry client"),
    ("opentelemetry-otlp", "the edge links no OTLP gRPC exporter"),
    ("tonic", "the edge links no OTLP gRPC exporter"),
];

/// The refused crates wash-runtime links with every feature off. The owner
/// accepted this list (finding `wamn-qt1t`, docs/plan/edge.md section 4.1).
const WASH_RUNTIME_LIST: &[&str] = &["async-nats", "opentelemetry-otlp", "redis", "tonic"];

/// The build host, then the Raspberry Pi 3B class box.
const TARGETS: &[Option<&str>] = &[None, Some("aarch64-unknown-linux-gnu")];

/// The crate names in `cargo tree -p wamn-edge -e normal`, plus `extra` flags.
fn normal_dependencies(target: Option<&str>, extra: &[&str]) -> BTreeSet<String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("wamn-edge lives two levels below the workspace root");
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(workspace)
        .args(["tree", "-p", "wamn-edge", "-e", "normal"]);
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    let output = command
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

fn target_name(target: Option<&str>) -> &str {
    target.unwrap_or("the build host")
}

#[test]
fn the_edge_links_no_refused_crate_outside_wash_runtime() {
    for target in TARGETS {
        let linked = normal_dependencies(*target, &["--prune", "wash-runtime"]);
        let violations: Vec<String> = REFUSED
            .iter()
            .filter(|(name, _)| linked.contains(*name))
            .map(|(name, rule)| {
                format!(
                    "wamn-edge links `{name}` for {}: {rule}",
                    target_name(*target)
                )
            })
            .collect();
        assert!(violations.is_empty(), "{}", violations.join("\n"));
    }
}

#[test]
fn wash_runtime_brings_exactly_the_accepted_refused_crates() {
    let accepted: BTreeSet<&str> = WASH_RUNTIME_LIST.iter().copied().collect();
    for target in TARGETS {
        let linked = normal_dependencies(*target, &[]);
        let present: BTreeSet<&str> = REFUSED
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| linked.contains(*name))
            .collect();
        assert_eq!(
            present,
            accepted,
            "the refused crates wamn-edge links through wash-runtime for {} changed; the \
             accepted list (finding wamn-qt1t) is {accepted:?}",
            target_name(*target)
        );
    }
}
