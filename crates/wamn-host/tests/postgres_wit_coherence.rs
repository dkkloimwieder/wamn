//! Drift guard tying the FROZEN `wamn:postgres@0.1.0` doc-of-record
//! (`docs/wamn-postgres.wit`) to every vendored copy of it. The `wamn:node`
//! (`wamn-node-sdk/tests/wit_coherence.rs`) and `wamn:jetstream`
//! (`jetstream_wit_coherence.rs`) precedents: editing the contract without
//! re-vendoring every copy (or drifting one copy on its own) fails a NAMED test
//! here instead of shipping skew that only surfaces as a cryptic linker error
//! when a guest fails to INSTANTIATE.
//!
//! Six... seven copies of `wamn:postgres/package.wit` are vendored under
//! `components/` and `crates/` (wit-bindgen resolves each guest/host's imports
//! from its OWN `wit/deps` tree, never from `docs/`). This guard:
//!
//!   1. WALKS `components/` and `crates/` for every `deps/wamn-postgres/
//!      package.wit`, and cross-checks the discovered set against the explicit
//!      [`EXPECTED_COPIES`] list BOTH ways — a removed/missing copy fails, and a
//!      NEW (eighth) copy fails with a message telling the author to register it
//!      here, so a future copy cannot dodge the guard.
//!   2. Asserts every copy's CODE (comment/blank-stripped, trimmed) is identical
//!      to `docs/wamn-postgres.wit`. This is the load-bearing check: the actual
//!      interface surface (types, records, variants, function signatures) must
//!      match everywhere, or a guest binds a different contract than the host.
//!   3. Asserts byte-identity WITHIN each cluster of copies that are byte-
//!      identical today ([`CLUSTER_A`], [`CLUSTER_B`]) — the strongest guard
//!      among identical copies, catching comment-only drift of one member away
//!      from its cluster.
//!
//! Why not one byte-identity check against docs? The copies are NOT byte-
//! identical to `docs/wamn-postgres.wit`: the doc comments diverge (the frozen
//! contract's prose still mentions the retired outbox, and two copies carry a
//! shorter doc-comment revision). Those differences are COMMENT-ONLY and known;
//! do not "fix" them by editing a WIT file. Hence check (2) compares CODE lines,
//! and check (3) keeps the byte-identical clusters honest about comments too.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/wamn-host; the repo root is two up.
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .expect("canonicalize repo root")
}

/// Comment- and blank-stripped, whitespace-trimmed code lines — the
/// `wit_coherence.rs` pattern. A doc-comment or blank-line difference is thereby
/// ignored; a change to any actual WIT declaration is not.
fn code_lines(wit: &str) -> Vec<&str> {
    wit.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .collect()
}

/// The complete, explicit set of vendored `wamn:postgres` contract copies. The
/// walk in [`all_vendored_copies_are_registered`] must discover exactly these;
/// adding a new guest that vendors the contract requires adding its path here.
const EXPECTED_COPIES: [&str; 7] = [
    "components/api-gateway/wit/deps/wamn-postgres/package.wit",
    "components/fixtures/pgprobe/wit/deps/wamn-postgres/package.wit",
    "components/flowrunner/wit/deps/wamn-postgres/package.wit",
    "components/materializer/wit/deps/wamn-postgres/package.wit",
    "components/poc-webhook-f1/wit/deps/wamn-postgres/package.wit",
    "crates/wamn-host/wit/deps/wamn-postgres/package.wit",
    "crates/wamn-node-guest/wit-caps/deps/wamn-postgres/package.wit",
];

/// The five copies that are byte-identical to one another today (the fuller
/// doc-comment revision). Byte-identity within the cluster is asserted so a
/// comment edit to one member fails here.
const CLUSTER_A: [&str; 5] = [
    "components/api-gateway/wit/deps/wamn-postgres/package.wit",
    "components/fixtures/pgprobe/wit/deps/wamn-postgres/package.wit",
    "components/flowrunner/wit/deps/wamn-postgres/package.wit",
    "components/poc-webhook-f1/wit/deps/wamn-postgres/package.wit",
    "crates/wamn-node-guest/wit-caps/deps/wamn-postgres/package.wit",
];

/// The two copies that are byte-identical to one another today (a shorter
/// doc-comment revision: they omit the schema-selection / freeze-status /
/// prepared-statement prose paragraphs the others carry). Same code, different
/// comments — kept byte-identical to EACH OTHER.
const CLUSTER_B: [&str; 2] = [
    "components/materializer/wit/deps/wamn-postgres/package.wit",
    "crates/wamn-host/wit/deps/wamn-postgres/package.wit",
];

/// Recursively collect every `deps/wamn-postgres/package.wit` under `dir`,
/// skipping any `target/` build directory, as repo-root-relative slash paths.
fn collect_copies(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            if path.file_name().and_then(|s| s.to_str()) == Some("target") {
                continue;
            }
            collect_copies(&path, root, out);
        } else if path.file_name().and_then(|s| s.to_str()) == Some("package.wit") {
            let parent = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str());
            let grandparent = path
                .parent()
                .and_then(Path::parent)
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str());
            if parent == Some("wamn-postgres") && grandparent == Some("deps") {
                out.push(
                    path.strip_prefix(root)
                        .expect("copy is under repo root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
}

fn discover_copies(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for top in ["components", "crates"] {
        collect_copies(&root.join(top), root, &mut out);
    }
    out.sort();
    out
}

/// The discovered vendored copies must equal [`EXPECTED_COPIES`] exactly — a new
/// copy fails (add it), a vanished copy fails (remove it). This is what stops a
/// future seventh/eighth guest from vendoring the contract unguarded.
#[test]
fn all_vendored_copies_are_registered() {
    let root = repo_root();
    let discovered = discover_copies(&root);

    let mut expected: Vec<String> = EXPECTED_COPIES.iter().map(|s| s.to_string()).collect();
    expected.sort();

    for found in &discovered {
        assert!(
            expected.contains(found),
            "found an UNREGISTERED wamn:postgres WIT copy: {found}\n\
             add it to EXPECTED_COPIES (and the appropriate cluster) in \
             crates/wamn-host/tests/postgres_wit_coherence.rs so the drift guard covers it"
        );
    }
    for want in &expected {
        assert!(
            discovered.contains(want),
            "expected wamn:postgres WIT copy {want} was not found on disk — \
             if it was intentionally removed, drop it from EXPECTED_COPIES"
        );
    }
    // Belt and suspenders: the CLUSTER_A/CLUSTER_B partition must cover exactly
    // the expected set, so cluster byte-identity (below) can never silently omit
    // a copy.
    let mut clustered: Vec<String> = CLUSTER_A
        .iter()
        .chain(CLUSTER_B.iter())
        .map(|s| s.to_string())
        .collect();
    clustered.sort();
    assert_eq!(
        clustered, expected,
        "CLUSTER_A + CLUSTER_B must partition EXPECTED_COPIES"
    );
}

/// Every vendored copy must carry the SAME interface code as the doc of record.
/// Doc comments may differ (known outbox mention / shorter revision); a change
/// to any real WIT declaration in any copy fails here.
#[test]
fn every_copy_shares_the_contract_code() {
    let root = repo_root();
    let docs = fs::read_to_string(root.join("docs/wamn-postgres.wit"))
        .expect("docs/wamn-postgres.wit reads");
    let docs_code = code_lines(&docs);

    for rel in EXPECTED_COPIES {
        let copy =
            fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel} reads: {e}"));
        assert_eq!(
            code_lines(&copy),
            docs_code,
            "{rel} drifted from docs/wamn-postgres.wit in a CODE line — the vendored \
             contract surface must stay identical (edit the doc of record AND re-vendor \
             every copy, or neither)"
        );
    }
}

/// Copies that are byte-identical today stay byte-identical WITHIN their cluster
/// — catching comment-only drift of a single member (which the code-line check
/// above deliberately ignores).
#[test]
fn byte_identical_clusters_stay_identical() {
    let root = repo_root();
    for (name, cluster) in [("CLUSTER_A", &CLUSTER_A[..]), ("CLUSTER_B", &CLUSTER_B[..])] {
        let first = fs::read_to_string(root.join(cluster[0]))
            .unwrap_or_else(|e| panic!("{}: {e}", cluster[0]));
        for rel in &cluster[1..] {
            let other = fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
            assert_eq!(
                first, other,
                "{rel} is no longer byte-identical to {} within {name} — a vendored \
                 copy drifted (a comment or code edit to one cluster member). Re-vendor \
                 so the cluster stays identical, or move the copy to its own cluster",
                cluster[0]
            );
        }
    }
}
