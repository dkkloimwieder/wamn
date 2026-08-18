//! Repository-level conformance for the caller-facing flow invocation ABI.

#[cfg(test)]
use wamn_flow_invocation::{
    Admitted, BeginResult, Failure, FlowError, InvokeResult, Rejection, Response,
};

#[cfg(test)]
const WIT: &str = include_str!("../../../crates/execution/flow-invocation/wit/package.wit");
#[cfg(test)]
const FLOWRUNNER_WIT: &str = include_str!("../../../components/execution/flowrunner/wit/world.wit");
/// The live node ABI (`wamn:node@0.2.0`, wamn-0h0g.16.2) — the seam the host
/// router invokes once per graph node. Included rather than read so that losing
/// the package fails the build outright, not just a test.
#[cfg(test)]
const NODE_WIT: &str = include_str!("../../../crates/execution/router/wit/package.wit");

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn flow_invocation_is_versioned_and_two_stage() {
        let code = WIT
            .lines()
            .filter(|line| !line.trim_start().starts_with("///"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(WIT.contains("package wamn:flow-invocation@0.1.0;"));
        // Both operations carry the wamn-0h0g.15.40 host-failure channel, so a
        // store outage answers the caller instead of trapping the flow-http
        // instance. The channel is separate from `rejection`, which stays the
        // decided pre-run refusal.
        assert!(WIT.contains(
            "begin: func(req: invoke-request) -> result<begin-result, invocation-error>;"
        ));
        assert!(WIT.contains(
            "wait: func(run-id: string, timeout-ms: u32) -> result<option<invoke-result>, invocation-error>;"
        ));
        assert!(WIT.contains("variant invocation-error {"));
        assert!(!WIT.contains("cancel:"));
        assert!(!WIT.contains("cancelled(failure)"));
        assert!(!code.contains("outcome-expired"));
        assert!(!code.contains("accepted("));
        assert!(!code.contains("pending("));
        // The node ABI is live again as wamn:node@0.2.0, so this is not a
        // tombstone: ingress and node execution are two contracts and must stay
        // two. Ingress answers a caller about a whole run; the node ABI drives
        // one pooled component instance through one graph hop. Naming it here
        // would put node-execution vocabulary on the caller-facing wire, where a
        // component-internal change would break external callers.
        // `node_abi_is_live_versioned_and_router_shaped` proves the package it
        // refuses to name actually exists, so this cannot go vacuous.
        assert!(
            !code.contains("wamn:node"),
            "the flow invocation contract must not name the node ABI"
        );
    }

    #[test]
    fn flowrunner_world_is_versioned_and_exports_only_run() {
        let code = FLOWRUNNER_WIT
            .lines()
            .filter(|line| !line.trim_start().starts_with("///"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(FLOWRUNNER_WIT.contains("package wamn:flowrunner@0.1.0;"));
        assert!(
            code.contains(
                "export run: func(run-id: string, payload: string) -> result<u32, string>;"
            )
        );
        assert_eq!(code.matches("export ").count(), 1);
    }

    #[test]
    fn admitted_and_rejected_results_cannot_share_run_identity() {
        let admitted = BeginResult::Admitted(Admitted {
            run_id: "run-1".to_string(),
        });
        let rejected = BeginResult::Rejected(Rejection {
            status: 409,
            code: "idempotency-scope-changed".to_string(),
        });

        assert!(matches!(admitted, BeginResult::Admitted(_)));
        assert!(matches!(rejected, BeginResult::Rejected(_)));
        assert!(
            !WIT.split("record rejection {")
                .nth(1)
                .expect("rejection record")
                .split('}')
                .next()
                .expect("rejection fields")
                .contains("run-id")
        );
    }

    #[test]
    fn stored_outcomes_keep_status_and_run_identity() {
        let response = InvokeResult::Responded(Response {
            run_id: "run-response".to_string(),
            body: "{}".to_string(),
            status_hint: Some(202),
        });
        let failed = InvokeResult::Failed(Failure {
            status: 400,
            error: FlowError {
                code: "authored-fail".to_string(),
                message: None,
                run_id: "run-failed".to_string(),
                flow_id: "flow-1".to_string(),
                flow_version: 3,
            },
        });

        assert!(matches!(response, InvokeResult::Responded(_)));
        assert!(matches!(
            failed,
            InvokeResult::Failed(Failure { status: 400, .. })
        ));
    }

    // --- the live node ABI (wamn-0h0g.16.2) ---
    //
    // The `wamn:node` ban above is a statement about a contract that EXISTS; the
    // two tests below are what make it a statement rather than a tautology. They
    // also give the node ABI the vendored-copy discipline of
    // `crates/platform/runtime/tests/postgres_wit_coherence.rs` from day one,
    // before the first copy is vendored — the copies that are hardest to guard
    // are the ones added while no guard is watching.

    /// Source of record for the live node ABI, repo-root-relative.
    ///
    /// STAGING: the intended owner is `crates/execution/router`, which does not
    /// exist yet, so the package is currently bound by nothing. When the router
    /// takes ownership at `crates/execution/router/wit/package.wit`, this
    /// constant and the `include_str!` above move together — and
    /// [`node_abi_source_and_included_copy_agree`] fails if only one of them does.
    const NODE_ABI_SOURCE: &str = "crates/execution/router/wit/package.wit";

    /// Every vendored copy of the node ABI, repo-root-relative. Empty today: the
    /// package is bound by nothing, so a copy has no reason to exist yet. The
    /// walk below cross-checks this list against disk BOTH ways, so the first
    /// consumer to vendor a copy must register it here or fail.
    const EXPECTED_NODE_ABI_COPIES: [&str; 0] = [];

    /// Tiers holding executable, bindable WIT. `docs/` is deliberately excluded:
    /// `docs/archive/contracts/wamn-node.wit` is the FROZEN 0.1.0 archive this
    /// package was revived from, not a vendored copy of it.
    const CODE_TIERS: [&str; 5] = ["components", "crates", "services", "test-support", "tests"];

    fn repo_root() -> PathBuf {
        // CARGO_MANIFEST_DIR is tests/conformance; the repo root is two up.
        fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .unwrap_or_else(|error| panic!("canonicalize repo root: {error}"))
    }

    /// Comment- and blank-stripped, whitespace-trimmed code lines. Doc-comment
    /// drift between copies is tolerated; a change to a real WIT declaration is
    /// not, because that is what a guest actually binds.
    fn code_lines(wit: &str) -> Vec<&str> {
        wit.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .collect()
    }

    /// Collect every `.wit` file under `dir` that declares the node package,
    /// as repo-root-relative slash paths. Discovery is by PACKAGE HEADER rather
    /// than by directory name, so a copy vendored under an off-convention
    /// directory cannot dodge the registry.
    fn collect_node_abi_copies(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                if path.file_name().and_then(|name| name.to_str()) == Some("target") {
                    continue;
                }
                collect_node_abi_copies(&path, root, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("wit")
                && fs::read_to_string(&path).is_ok_and(|text| text.contains("package wamn:node@"))
            {
                let relative = path
                    .strip_prefix(root)
                    .expect("copy is under repo root")
                    .to_string_lossy()
                    .replace('\\', "/");
                // The source of record lives inside a code tier, so the walk
                // finds it too. A copy is by definition not the source.
                if relative != NODE_ABI_SOURCE {
                    out.push(relative);
                }
            }
        }
    }

    #[test]
    fn node_abi_is_live_versioned_and_router_shaped() {
        let code = code_lines(NODE_WIT).join("\n");

        assert!(NODE_WIT.contains("package wamn:node@0.2.0;"));
        // 0.2.0, not a 0.1.x amendment: the frozen 0.1.0 package's own
        // compatibility rule sends every breaking change to 0.2, and this
        // revision removed types and changed the signature.
        assert!(!code.contains("package wamn:node@0.1.0;"));

        // The single operation the router invokes per graph node.
        assert!(code.contains(
            "run: func(ctx: node-context, input: json) -> result<emission, node-error>;"
        ));
        assert!(code.contains("world node {"));
        assert!(code.contains("export handler;"));

        // The identity the router supplies. `(delivery-id, node-id)` is the
        // stable idempotency basis under at-least-once redelivery; the wiring
        // pair scopes it to the graph revision that asked for the effect.
        for field in [
            "wiring-id: string,",
            "wiring-version: u32,",
            "node-id: string,",
            "delivery-id: string,",
            "config: json,",
        ] {
            assert!(code.contains(field), "node-context lost {field:?}");
        }
        // OTel is the record in this model and there are no durable node facts,
        // so a node that cannot propagate trace context breaks the only evidence.
        assert!(code.contains("traceparent: option<string>,"));
        assert!(code.contains("tracestate: option<string>,"));
        // What makes `cancelled` reachable without a cancellation import.
        assert!(code.contains("deadline-ms: option<u64>,"));

        // Every arm drives a different router action: retry, honour the source's
        // own delay, error edge, dead-letter, and "nothing failed".
        for arm in [
            "retryable(error-detail),",
            "rate-limited(rate-limit-detail),",
            "terminal(error-detail),",
            "invalid-input(error-detail),",
            "cancelled,",
        ] {
            assert!(code.contains(arm), "node-error lost {arm:?}");
        }
        assert!(
            code.contains("port: option<string>,"),
            "emission lost its port"
        );

        // Retired with the frames model and the payload store. Each of these
        // returning would re-import a subsystem this revision exists to shed.
        for retired in [
            "payload-ref",
            "framing",
            "streamed(",
            "interface payloads",
            "interface credentials",
            "interface control",
            "run-id:",
            "flow-id:",
            "flow-version:",
            "attempt:",
            "world stream-node",
            "world http-node",
        ] {
            assert!(
                !code.contains(retired),
                "the node ABI re-admitted retired vocabulary {retired:?}"
            );
        }
    }

    /// The `include_str!` path and [`NODE_ABI_SOURCE`] must name the same file.
    /// Without this, moving the package and updating only the `include_str!`
    /// still compiles while the registry walk silently guards a stale path.
    #[test]
    fn node_abi_source_and_included_copy_agree() {
        let path = repo_root().join(NODE_ABI_SOURCE);
        let on_disk = fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "the node ABI source of record must exist at {NODE_ABI_SOURCE}, \
                 but it did not read: {error}. If the package moved, update BOTH \
                 the include_str! above and NODE_ABI_SOURCE"
            )
        });
        assert_eq!(
            on_disk, NODE_WIT,
            "{NODE_ABI_SOURCE} is not the file the include_str! above compiled in \
             — the two paths have drifted apart"
        );
    }

    /// The discovered vendored copies must equal [`EXPECTED_NODE_ABI_COPIES`]
    /// exactly — an unregistered copy fails (register it), a vanished one fails
    /// (drop it) — and every registered copy must carry the source of record's
    /// code. This is what stops a future consumer from binding a drifted node
    /// ABI, which otherwise surfaces only as a cryptic instantiation failure.
    #[test]
    fn all_vendored_node_abi_copies_are_registered_and_match_the_source() {
        let root = repo_root();
        let mut discovered = Vec::new();
        for tier in CODE_TIERS {
            collect_node_abi_copies(&root.join(tier), &root, &mut discovered);
        }
        discovered.sort();

        let mut expected: Vec<String> = EXPECTED_NODE_ABI_COPIES
            .iter()
            .map(|copy| (*copy).to_string())
            .collect();
        expected.sort();

        for found in &discovered {
            assert!(
                expected.contains(found),
                "found an UNREGISTERED wamn:node WIT copy: {found}\n\
                 add it to EXPECTED_NODE_ABI_COPIES in \
                 tests/conformance/src/invocation.rs so the drift guard covers it"
            );
        }
        for want in &expected {
            assert!(
                discovered.contains(want),
                "expected wamn:node WIT copy {want} was not found on disk — if it \
                 was intentionally removed, drop it from EXPECTED_NODE_ABI_COPIES"
            );
        }

        let source_code = code_lines(NODE_WIT);
        for rel in &expected {
            let copy = fs::read_to_string(root.join(rel))
                .unwrap_or_else(|error| panic!("{rel} reads: {error}"));
            assert_eq!(
                code_lines(&copy),
                source_code,
                "{rel} drifted from {NODE_ABI_SOURCE} in a CODE line — a vendored \
                 contract surface must stay identical to its source of record \
                 (edit the source AND re-vendor every copy, or neither)"
            );
        }
    }
}
