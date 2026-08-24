//! Repository-level conformance for the live node invocation ABI.

/// The live node ABI (`wamn:node@0.1.0`, wamn-0h0g.16.2) — the seam the host
/// router invokes once per graph node. Included rather than read so that losing
/// the package fails the build outright, not just a test.
#[cfg(test)]
const NODE_WIT: &str = include_str!("../../../crates/execution/router/wit/package.wit");

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    // The live node ABI has the vendored-copy discipline of
    // `crates/platform/runtime/tests/postgres_wit_coherence.rs` from day one,
    // before the first copy is vendored — the copies that are hardest to guard
    // are the ones added while no guard is watching.

    /// Source of record for the live node ABI, repo-root-relative.
    ///
    /// The router owns it and both sides bind it:
    /// `crates/execution/host/src/router_driver.rs` generates the host bindings
    /// from `../router/wit` (world `node`), and the library components export
    /// `wamn:node/handler@0.1.0`. This constant and the `include_str!` above must
    /// move together — [`node_abi_source_and_included_copy_agree`] fails if only
    /// one of them does.
    const NODE_ABI_SOURCE: &str = "crates/execution/router/wit/package.wit";

    /// Every vendored copy of the node ABI, repo-root-relative. The walk below
    /// cross-checks this list against disk BOTH ways.
    const EXPECTED_NODE_ABI_COPIES: [&str; 2] = [
        "components/library/http-request/wit/deps/wamn-node/package.wit",
        "components/library/transform/wit/deps/wamn-node/package.wit",
    ];

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

        assert!(NODE_WIT.contains("package wamn:node@0.1.0;"));
        // @0.1.0, per the MVP version-identity rule (wamn-0h0g.16.10): this
        // package supersedes the archived 0.1.0 wholesale rather than evolving
        // it, so the archive's "breaking changes wait for 0.2" clause does not
        // bind a package the archive has no live consumer for. The version
        // literal therefore no longer separates the live shape from the archived
        // one — the retired-vocabulary sweep at the end of this test is what
        // does, and it must stay for that reason.
        assert!(
            NODE_WIT.contains("SUPERSEDES the archived `wamn:node@0.1.0`"),
            "the package header must keep recording that it supersedes the \
             archived 0.1.0 wholesale — without it, two different shapes sit \
             under one version with nothing saying so"
        );

        // The single operation the router invokes per graph node.
        assert!(code.contains(
            "run: func(ctx: node-context, input: json) -> result<emission, node-error>;"
        ));
        assert!(code.contains("world node {"));
        assert!(code.contains("export handler;"));

        // The identity and target input the router supplies. Occurrence keeps
        // merge/loop visits distinct while retries retain one visit identity;
        // the wiring pair scopes it to the graph revision that asked for the
        // effect.
        for field in [
            "wiring-id: string,",
            "wiring-version: u32,",
            "node-id: string,",
            "delivery-id: string,",
            "input-port: option<string>,",
            "occurrence: u32,",
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
        let node_context = code
            .split_once("record node-context {")
            .expect("node-context exists")
            .1
            .split_once('}')
            .expect("node-context closes")
            .0;
        assert!(
            !node_context.contains("operation:"),
            "operation selects the admitted digest and must not enter handler.run"
        );
        assert!(
            !node_context.contains("context:"),
            "the retired mutable delivery context must not re-enter node-context"
        );
        let emission = code
            .split_once("record emission {")
            .expect("emission exists")
            .1
            .split_once('}')
            .expect("emission closes")
            .0;
        assert!(
            !emission.contains("context:"),
            "a success must not return the retired mutable delivery context"
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
