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

    /// Source of record for the live node ABI, repo-root-relative.
    ///
    /// The router owns it and both sides bind it:
    /// `crates/execution/host/src/router_driver.rs` generates the host bindings
    /// from `../router/wit` (world `node`), and admitted component exports must
    /// structurally match its `handler.run` signature. This constant and the
    /// `include_str!` above must move together —
    /// [`node_abi_source_and_included_copy_agree`] fails if only one does.
    const NODE_ABI_SOURCE: &str = "crates/execution/router/wit/package.wit";

    fn repo_root() -> PathBuf {
        // CARGO_MANIFEST_DIR is tests/conformance; the repo root is two up.
        fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .unwrap_or_else(|error| panic!("canonicalize repo root: {error}"))
    }

    /// Ignore whitespace and comments when asserting the source contract shape.
    fn code_lines(wit: &str) -> Vec<&str> {
        wit.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .collect()
    }

    /// Collect every `.wit` file that declares the node package by its header.
    /// Return paths relative to the repository root.
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
                && fs::read_to_string(&path).is_ok_and(|text| {
                    text.lines()
                        .any(|line| line.trim().starts_with("package wamn:node@"))
                })
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
        // The structural operation shape the router invokes per graph node.
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
        //
        // wamn-0h0g.15.139 audited these and left them bare, but the safety is
        // load-bearing and not local: the haystack is `code`, already stripped by
        // `code_lines` above. `framing` is an ordinary English word and over the
        // raw file it would forbid a doc comment explaining that the ABI does no
        // framing — the `flowrunner` failure of wamn-0h0g.15.131. Bare is correct
        // here ONLY because comments are gone. Do not re-point this loop at
        // `NODE_WIT` or any other unstripped text.
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
    /// still compiles while the discovery walk silently guards a stale path.
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

    /// Every discovered copy must contain the complete source contract bytes.
    #[test]
    fn all_vendored_node_abi_copies_match_the_source() {
        let root = repo_root();
        let mut discovered = Vec::new();
        for top in [
            "apps",
            "components",
            "crates",
            "services",
            "test-support",
            "tests",
        ] {
            collect_node_abi_copies(&root.join(top), &root, &mut discovered);
        }
        discovered.sort();

        for rel in &discovered {
            let copy =
                fs::read(root.join(rel)).unwrap_or_else(|error| panic!("{rel} reads: {error}"));
            assert_eq!(
                copy,
                NODE_WIT.as_bytes(),
                "{rel} drifted from {NODE_ABI_SOURCE}; re-vendor the complete package"
            );
        }
    }
}
