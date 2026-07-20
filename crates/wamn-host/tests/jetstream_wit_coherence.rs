//! Drift guard tying the `wamn:jetstream@0.1.0` doc-of-record
//! (`docs/wamn-jetstream.wit`) to the built copy the host bindgen compiles
//! (`crates/wamn-host/wit/deps/wamn-jetstream/package.wit`). The
//! `wamn:postgres` / `wamn:node` committed-contract precedent: editing one copy
//! without the other fails a named test instead of shipping skew. The two are
//! kept BYTE-IDENTICAL (like `docs/wamn-postgres.wit` and its vendored copy).

use std::fs;
use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn docs_wit_matches_built_copy() {
    let docs = fs::read_to_string(root().join("../../docs/wamn-jetstream.wit"))
        .expect("docs/wamn-jetstream.wit reads");
    let built = fs::read_to_string(root().join("wit/deps/wamn-jetstream/package.wit"))
        .expect("wit/deps/wamn-jetstream/package.wit reads");
    assert_eq!(
        docs, built,
        "docs/wamn-jetstream.wit and the built wit/deps copy drifted — they must \
         stay byte-identical (edit both, or neither)"
    );
}

#[test]
fn component_copies_match_docs() {
    // Every guest that binds wamn:jetstream vendors its OWN byte-identical copy
    // of the contract under wit/deps (wit-bindgen resolves from there, not from
    // docs). The materializer (l5i9.17, consumer + doorbell) and the js-sample
    // (l5i9.57, consumer + producer — the first producer importer) each carry
    // one; editing docs without re-vendoring both fails HERE rather than
    // shipping a guest built against a stale contract.
    let docs = fs::read_to_string(root().join("../../docs/wamn-jetstream.wit"))
        .expect("docs/wamn-jetstream.wit reads");
    for copy in [
        "../../components/materializer/wit/deps/wamn-jetstream/package.wit",
        "../../components/samples/js-sample/wit/deps/wamn-jetstream/package.wit",
    ] {
        let vendored =
            fs::read_to_string(root().join(copy)).unwrap_or_else(|e| panic!("{copy} reads: {e}"));
        assert_eq!(
            docs, vendored,
            "{copy} drifted from docs/wamn-jetstream.wit — the vendored guest \
             copy must stay byte-identical (edit both, or neither)"
        );
    }
}

#[test]
fn contract_declares_the_mvp_surface() {
    // The materializer (l5i9.17) binds exactly these; a rename/removal of any
    // load-bearing line is a breaking change that must move the plugin too.
    let docs = fs::read_to_string(root().join("../../docs/wamn-jetstream.wit"))
        .expect("docs/wamn-jetstream.wit reads");
    for needle in [
        "package wamn:jetstream@0.1.0;",
        "record consumer-config {",
        "durable: string,",
        "filter-subject: string,",
        "ack-wait-ms: u64,",
        "max-deliver: u32,",
        "fetch: func(max-messages: u32, expires-ms: u64) -> result<list<message>, js-error>;",
        "ack: func() -> result<_, js-error>;",
        "nack: func(delay-ms: u64) -> result<_, js-error>;",
        "term: func() -> result<_, js-error>;",
        "stream-seq: u64,",
        "delivered: u64,",
        "publish: func(subject: string, headers: list<header>, body: list<u8>) -> result<publish-ack, js-error>;",
        // l5i9.17: the post-commit doorbell takeover — run-id only; the tenant
        // is host-derived from the workload's wamn.tenant, never a parameter.
        "ring: func(run-id: string) -> result<_, js-error>;",
    ] {
        assert!(
            docs.contains(needle),
            "wamn:jetstream contract is missing the MVP line: {needle:?}"
        );
    }
}
