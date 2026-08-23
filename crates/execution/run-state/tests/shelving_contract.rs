//! The shelving contract, as a test (wamn-0h0g.19.7, wamn-0h0g.20.5 §2).
//!
//! `docs/exe-model.md` "The boundary" promises that the premium durable tier,
//! when sold, "slots in at path 3's claim (classifier + ledger re-enabled per
//! class) WITH ZERO CHANGES TO PATHS 1-2". Prose cannot hold that: once the
//! crash floor is behind a class gate, "we preserved the contract" becomes
//! unfalsifiable the moment anyone forgets it. This file is the falsifiable
//! form — it reds if paths 1-2 ever grow a durability dependency.
//!
//! WHAT PATHS 1-2 ARE, in code. Path 1 is hot HTTP routes: the routing plugin
//! resolves an attachment and the router walks the wiring inline on pooled
//! instances — no run row, no queue row, no Postgres in the path. Path 2 is
//! streams: durable pull consumers hand a delivery to the same walk. BOTH RUN
//! THE SAME ROUTER AND NOTHING ELSE OF THIS CRATE'S, which is exactly why the
//! invariant is checkable: `crates/execution/router` is the whole surface, and
//! the class lives here in `wamn-run-state`.
//!
//! Path 3 — triggered automations — is the one path carrying a run row, and it
//! is where the gate sits. That asymmetry is the contract.

/// The router crate's manifest: paths 1-2's entire dependency surface.
const ROUTER_MANIFEST: &str = include_str!("../../router/Cargo.toml");

/// Every router source file. A new module joins this list or the guard below
/// stops covering it — and the coverage check is what makes that visible.
const ROUTER_SOURCES: &[(&str, &str)] = &[
    ("lib.rs", include_str!("../../router/src/lib.rs")),
    ("outcome.rs", include_str!("../../router/src/outcome.rs")),
    (
        "resolution.rs",
        include_str!("../../router/src/resolution.rs"),
    ),
    ("retry.rs", include_str!("../../router/src/retry.rs")),
    ("terminal.rs", include_str!("../../router/src/terminal.rs")),
    ("walk.rs", include_str!("../../router/src/walk.rs")),
    ("wiring.rs", include_str!("../../router/src/wiring.rs")),
];

/// Vocabulary that only exists because the crash floor exists. None of it may
/// reach the walk paths 1-2 run.
///
/// Every entry is an IDENTIFIER or a quoted SQL literal, never a bare English
/// word: `durable` on its own matches the phrase "no durable recovery" in the
/// walk's own module doc, and a guard that reds on prose is a guard people
/// delete.
const CRASH_FLOOR_VOCABULARY: &[&str] = &[
    "durability_class",
    "DurabilityClass",
    "'durable'",
    "effect_attempts",
    "effect_attempt",
    "effect-uncertain",
    "EffectUncertain",
    "ProductionClaimClass",
    "run_queue",
    "lease_expires_at",
];

#[test]
fn paths_one_and_two_carry_no_durability_dependency() {
    for (name, source) in ROUTER_SOURCES {
        for term in CRASH_FLOOR_VOCABULARY {
            assert!(
                !source.contains(term),
                "router/src/{name} grew a crash-floor dependency on `{term}`: \
                 enabling the premium tier at path 3's claim must change paths \
                 1-2 by ZERO (docs/exe-model.md, 'The boundary')"
            );
        }
    }
}

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

#[test]
fn the_terminal_vocabulary_is_class_independent() {
    // The emit boundary is where durability is BOUGHT, so the terminal verdicts
    // are the most tempting place to leak a tier into paths 1-2. There are three
    // verdicts and two terminals, and neither set may grow a class arm.
    let terminal = ROUTER_SOURCES
        .iter()
        .find(|(name, _)| *name == "terminal.rs")
        .expect("the terminal module is covered")
        .1;
    for required in [
        "pub const DEDUP_ID_FIELD: &str = \"dedup-id\";",
        "Respond { payload: Value, node_id: String },",
        "Emit {\n        event: Value,\n        dedup_id: String,\n        entity: String,\n        operation: Op,\n    },",
        "Discard,",
    ] {
        assert!(
            terminal.contains(required),
            "the terminal vocabulary lost `{required}`"
        );
    }
    // The dedup id is AUTHOR-SUPPLIED and the router only CARRIES it: what
    // dedups on it is the boundary, whose machinery is this crate's. A router
    // that started deduping would be path 1-2 code doing path 3's job.
    for forbidden in ["idempotency", "admission", "admit"] {
        assert!(
            !terminal.contains(forbidden),
            "the router started owning `{forbidden}`; the boundary dedups, the \
             router carries"
        );
    }
}

#[test]
fn the_gate_is_data_not_a_build_flag() {
    // wamn-0h0g.20's grounded fact: both tiers ship in ONE image, so a Cargo
    // feature cannot express the class — a feature cannot let two tiers coexist
    // in one binary. Turning the premium tier on must therefore be a per-run
    // DATA change, and nothing about the class may be conditionally compiled.
    for (path, source) in [
        ("durability.rs", include_str!("../src/durability.rs")),
        ("queue/claim.rs", include_str!("../src/queue/claim.rs")),
        ("queue/sql.rs", include_str!("../src/queue/sql.rs")),
    ] {
        for line in source.lines().filter(|line| line.contains("durability")) {
            assert!(
                !line.contains("cfg(feature"),
                "{path} put the class behind a build flag: {line}"
            );
        }
    }
}
