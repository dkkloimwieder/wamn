//! The shelving contract, as a test (wamn-0h0g.19.7, wamn-0h0g.20.5 §2).
//!
//! `docs/exe-model.md` "The boundary" promises that the premium durable tier,
//! when sold, "slots in at path 3's claim (classifier + ledger re-enabled per
//! class) WITH ZERO CHANGES TO PATHS 1-2". Prose cannot hold that: once the
//! crash floor is behind a class gate, "we preserved the contract" becomes
//! unfalsifiable the moment anyone forgets it.
//!
//! WHAT PATHS 1-2 ARE, in code. Path 1 is hot HTTP routes: the routing plugin
//! resolves an attachment and the router walks the wiring inline on pooled
//! instances — no run row, no queue row, no Postgres in the path. Path 2 is
//! streams: durable pull consumers hand a delivery to the same walk. BOTH RUN
//! THE SAME ROUTER AND NOTHING ELSE OF THIS CRATE'S, which is exactly why the
//! invariant is checkable.
//!
//! Path 3 — triggered automations — is the one path carrying a run row, and it
//! is where the gate sits. That asymmetry is the contract.
//!
//! # How this is enforced now (wamn-hopk)
//!
//! It used to be enforced by reading the router's source files as text,
//! stripping their comments, and grepping the remainder for crash-floor
//! identifiers. That is retired: a rule enforced by hand-rolled text processing
//! over our own files grows parsers to fix its own false positives, and it
//! cannot tell a reference from a mention.
//!
//! THE COMPILER ENFORCES IT INSTEAD, and more strongly than any scan could.
//! `crates/execution/router` does not LINK `wamn-run-state`, `wamn-runtime`, or
//! any Postgres client, so the class is not merely unreferenced by paths 1-2 —
//! it is unnameable from them, and a reference is `E0433` at build time. The
//! one assertion left here reads the router's MANIFEST, which is the fact that
//! makes the compile-time wall true; it is a dependency check, not a source
//! scan.

/// The router crate's manifest: paths 1-2's entire dependency surface.
const ROUTER_MANIFEST: &str = include_str!("../../router/Cargo.toml");

#[test]
fn the_router_cannot_reach_the_run_plane_at_all() {
    // The strongest form of the invariant, and the one a later edit cannot talk
    // its way past: paths 1-2 do not LINK the crate the class lives in, so no
    // amount of forgetting can make them read it.
    for forbidden in [
        "wamn-run-state",
        "wamn_run_state",
        "wamn-runtime",
        "tokio-postgres",
        "deadpool-postgres",
    ] {
        assert!(
            !ROUTER_MANIFEST.contains(forbidden),
            "the router took a dependency on `{forbidden}`; paths 1-2 have no \
             run row, no queue row, and no Postgres in the path"
        );
    }
}

// wamn-hopk R5: the class-is-data guard read three src files as text. It is
// deleted with every other source scan. R1 replaced its subject: the shelf now
// sits behind the default-off `durable-tier` feature, so what it asserted about
// conditional compilation is now the compiler's business, not a grep's.
