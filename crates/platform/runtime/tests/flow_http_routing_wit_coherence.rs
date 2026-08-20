//! Drift guard for the twice-vendored `wamn:flow-http-routing@0.1.0` contract
//! (wamn-0h0g.15.120).
//!
//! Unlike `wamn:jetstream` and `wamn:connection` this package has no
//! doc-of-record under `docs/archive/contracts/`. It exists only as two vendored
//! copies, one per bindgen that compiles it: the host plugin's
//! (`crates/platform/runtime/wit/deps/`, see `plugins/flow_http_routing.rs`) and
//! the flow-http guest's (`components/ingress/flow-http/wit/deps/`, see
//! `src/guest.rs`). Neither outranks the other, so the surface is pinned in both,
//! and the two are kept BYTE-IDENTICAL on the `wamn:jetstream` precedent: editing
//! one without the other fails a named test here instead of shipping a host and a
//! guest that disagree about the wire.

use std::fs;
use std::path::{Path, PathBuf};

const HOST_COPY: &str = include_str!("../wit/deps/wamn-flow-http-routing/package.wit");
const HTTP_COPY: &str = include_str!(
    "../../../../components/ingress/flow-http/wit/deps/wamn-flow-http-routing/package.wit"
);
const HOST_WORLD: &str = include_str!("../wit/world.wit");
const HTTP_WORLD: &str = include_str!("../../../../components/ingress/flow-http/wit/world.wit");
const PLUGIN: &str = include_str!("../src/plugins/flow_http_routing.rs");
const HOST: &str = include_str!("../../../../services/host/src/host.rs");

/// Every vendored copy of the package, repository-relative and sorted.
///
/// The two `include_str!` consts above only prove that the copies THIS guard
/// names agree. A third copy — a fixture guest that binds `routing`, say — would
/// be a copy nobody compares, which is the failure this bead exists for, so the
/// tree is enumerated as well (the `wamn:connection` guard does the same).
const REGISTERED_COPIES: [&str; 2] = [
    "components/ingress/flow-http/wit/deps/wamn-flow-http-routing/package.wit",
    "crates/platform/runtime/wit/deps/wamn-flow-http-routing/package.wit",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root canonicalizes")
}

fn collect_copies(dir: &Path, root: &Path, found: &mut Vec<String>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|error| panic!("{} reads: {error}", dir.display()));
    for entry in entries {
        let entry = entry.expect("directory entry reads");
        let path = entry.path();
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | "target")
            ) {
                continue;
            }
            collect_copies(&path, root, found);
        } else if path.file_name().and_then(|name| name.to_str()) == Some("package.wit") {
            let source = fs::read_to_string(&path).expect("candidate WIT reads");
            if source.contains("package wamn:flow-http-routing@") {
                found.push(
                    path.strip_prefix(root)
                        .expect("vendored WIT is within repository")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
}

/// The body of one item declared inside `interface routing`, braces excluded.
///
/// Every record and enum in the package sits at one indentation level inside the
/// interface and holds only fields, so its block closes at exactly `"\n  }"` —
/// the first such sequence after the header. Comparing a whole block rather than
/// a line at a time is what makes an ADDED or REMOVED field fail, not only a
/// renamed one.
fn item_body<'a>(package: &'a str, header: &str) -> &'a str {
    let (_, body) = package
        .split_once(header)
        .unwrap_or_else(|| panic!("wamn:flow-http-routing package declares {header:?}"));
    body.split_once("\n  }")
        .map(|(body, _)| body.trim())
        .unwrap_or_else(|| panic!("{header:?} closes"))
}

#[test]
fn the_two_vendored_copies_stay_byte_identical() {
    assert_eq!(
        HOST_COPY, HTTP_COPY,
        "crates/platform/runtime/wit/deps/wamn-flow-http-routing/package.wit and \
         components/ingress/flow-http/wit/deps/wamn-flow-http-routing/package.wit drifted — \
         the host plugin and the guest bind the same package from their own vendored copy \
         and must stay byte-identical (edit both, or neither)"
    );
}

#[test]
fn every_vendored_copy_of_the_package_is_one_this_guard_compares() {
    let root = repo_root();
    let mut found = Vec::new();
    for top in ["components", "crates", "services"] {
        collect_copies(&root.join(top), &root, &mut found);
    }
    found.sort();

    assert_eq!(
        found, REGISTERED_COPIES,
        "a vendored wamn:flow-http-routing copy is not one of the two this guard pins — \
         register it here, or it drifts unobserved"
    );
}

#[test]
fn route_definition_carries_exactly_the_fields_the_host_fills_and_the_guest_reads() {
    // The host fills every one of these in `route_definition` and the guest
    // destructures every one of them in `route_definition`, so a field added or
    // dropped on one side is a compile error on the other — but only once both
    // copies move. This is the pin that makes the WIT edit itself visible.
    //
    // The widths are load-bearing beyond their names: `status`/`u16` types the
    // plugin's `UNSUPPORTED_POLICY_STATUS`, and the two `u64` ceilings are what
    // the guest narrows with `usize::try_from`, which is why the host answers
    // them with `u32::MAX` and not `u64::MAX`.
    const ROUTE_DEFINITION: &str = r#"attachment-id: string,
    catalog-version: u64,
    definition-hash: string,
    host: string,
    path: string,
    method: string,
    enabled: bool,
    auth-policy: string,
    mappings: list<mapping>,
    input-schema: string,
    idempotency-required: bool,
    body-limit: u64,
    mapped-limit: u64,
    deadline-override: option<u64>,"#;

    for copy in [HOST_COPY, HTTP_COPY] {
        assert_eq!(
            item_body(copy, "record route-definition {"),
            ROUTE_DEFINITION
        );
    }
}

#[test]
fn the_three_bound_functions_and_the_refusal_shape_are_unchanged() {
    const AUTH_REJECTION: &str = r#"status: u16,
    code: string,"#;

    for copy in [HOST_COPY, HTTP_COPY] {
        assert!(copy.starts_with("package wamn:flow-http-routing@0.1.0;\n"));
        for signature in [
            // `routes` refuses with a bare string: the plugin's `NoRelease` is
            // stringified at this boundary, so a typed error channel here would
            // move the refusal, not just rename it.
            "routes: func(method: string, authority: string) -> result<list<route-definition>, string>;",
            // `authenticate` takes the policy the SELECTED route supplied; a
            // signature that reached for the attachment itself would let the
            // guest choose its own policy.
            "authenticate: func(policy: string, headers: list<header>) -> result<string, auth-rejection>;",
            // No parameters: the host cannot see the inbound socket, so there is
            // nothing for a caller to pass that would make this answerable.
            "caller-connected: func() -> bool;",
        ] {
            assert!(
                copy.contains(signature),
                "vendored wamn:flow-http-routing copy no longer declares {signature:?}"
            );
        }
        assert_eq!(item_body(copy, "record auth-rejection {"), AUTH_REJECTION);
    }
}

#[test]
fn the_mapping_vocabulary_is_exactly_the_arms_both_sides_match_on() {
    // The host maps authored JSON onto these arms in `input_mapping` and the
    // guest maps them onto its own enums in `route_definition`; both matches are
    // exhaustive, so an arm added to the WIT alone compiles nothing.
    const MAPPING_SOURCE: &str = r#"body,
    path,
    query,
    header,"#;
    const CARDINALITY: &str = r#"one,
    many,"#;
    const MAPPING: &str = r#"%from: mapping-source,
    name: string,
    to: string,
    optional: bool,
    cardinality: cardinality,"#;
    const HEADER: &str = r#"name: string,
    value: string,"#;

    for copy in [HOST_COPY, HTTP_COPY] {
        assert_eq!(item_body(copy, "enum mapping-source {"), MAPPING_SOURCE);
        assert_eq!(item_body(copy, "enum cardinality {"), CARDINALITY);
        assert_eq!(item_body(copy, "record mapping {"), MAPPING);
        assert_eq!(item_body(copy, "record header {"), HEADER);
    }
}

#[test]
fn both_worlds_import_the_routing_interface_and_the_production_host_links_it() {
    const IMPORT: &str = "import wamn:flow-http-routing/routing@0.1.0;";

    // The bindgen world name and the WIT world name are the same string in two
    // files; the namespace guard is a third spelling of the package identity, and
    // it decides whether `add_to_linker` fires at all — a package rename that
    // missed it would produce a plugin whose import resolves to nothing.
    assert!(HOST_WORLD.contains("world flow-http-routing-plugin {"));
    assert_eq!(HOST_WORLD.matches(IMPORT).count(), 1);
    assert!(PLUGIN.contains("world: \"flow-http-routing-plugin\","));
    assert!(PLUGIN.contains("\"wamn:flow-http-routing/routing@0.1.0\""));
    assert!(PLUGIN.contains("contains(\"wamn\", \"flow-http-routing\", &[\"routing\"])"));
    assert!(PLUGIN.contains("routing::add_to_linker"));

    assert!(HTTP_WORLD.contains("world flow-http {"));
    assert_eq!(HTTP_WORLD.matches(IMPORT).count(), 1);

    // The WASH host, not the flowrunner in-process host: the plugin is only
    // reachable from the process that runs the flow-http workload.
    assert!(HOST.contains("FlowHttpRouting::new(release.clone())"));
}
