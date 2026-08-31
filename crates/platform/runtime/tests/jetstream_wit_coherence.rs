//! Drift guard tying the `wamn:jetstream@0.1.0` built host contract to every
//! guest-vendored copy. Archived provenance is deliberately not a live input.

use std::fs;
use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn component_copies_match_the_built_contract() {
    // Every guest that binds wamn:jetstream vendors its OWN byte-identical copy
    // of the contract under wit/deps. The materializer carries one; editing the
    // host contract without re-vendoring it fails here.
    let built = fs::read_to_string(root().join("wit/deps/wamn-jetstream/package.wit"))
        .expect("wit/deps/wamn-jetstream/package.wit reads");
    let copy = "../../../components/execution/materializer/wit/deps/wamn-jetstream/package.wit";
    let vendored =
        fs::read_to_string(root().join(copy)).unwrap_or_else(|e| panic!("{copy} reads: {e}"));
    assert_eq!(
        built, vendored,
        "{copy} drifted from the host's built wamn:jetstream contract"
    );
}

#[test]
fn contract_declares_the_mvp_surface() {
    // The materializer (l5i9.17) binds exactly these; a rename/removal of any
    // load-bearing line is a breaking change that must move the plugin too.
    let built = fs::read_to_string(root().join("wit/deps/wamn-jetstream/package.wit"))
        .expect("wit/deps/wamn-jetstream/package.wit reads");
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
        "dead-letter: func(reason: string) -> result<_, js-error>;",
        "bind-registration: func(package-id: string, registration-id: string, config: consumer-config) -> result<durable-consumer, js-error>;",
        "stream-seq: u64,",
        "delivered: u64,",
        "publish: func(subject: string, headers: list<header>, body: list<u8>) -> result<publish-ack, js-error>;",
        // l5i9.17: the post-commit doorbell takeover — run-id only; the tenant
        // is host-derived from the workload's wamn.tenant, never a parameter.
        "ring: func(run-id: string) -> result<_, js-error>;",
    ] {
        assert!(
            built.contains(needle),
            "wamn:jetstream contract is missing the MVP line: {needle:?}"
        );
    }
}
