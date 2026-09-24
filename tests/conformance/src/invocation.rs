//! Repository-level conformance for the live node invocation ABI.

/// The live node ABI (`wamn:node@0.1.0`, wamn-0h0g.16.2) — the seam the host
/// router invokes once per graph node. Included rather than read so that losing
/// the package fails the build outright, not just a test.
#[cfg(test)]
const NODE_WIT: &str = include_str!("../../../crates/execution/workflow/router/wit/package.wit");

#[cfg(test)]
mod tests {
    use wit_parser::{Resolve, Type, TypeDefKind, WorldItem};

    use super::NODE_WIT;

    /// The WIT spelling of one resolved type, enough to compare the ABI shape.
    fn spell(resolve: &Resolve, ty: &Type) -> String {
        match ty {
            Type::String => "string".to_owned(),
            Type::U32 => "u32".to_owned(),
            Type::U64 => "u64".to_owned(),
            Type::Id(id) => {
                let definition = &resolve.types[*id];
                match (&definition.name, &definition.kind) {
                    (Some(name), _) => name.clone(),
                    (None, TypeDefKind::Option(inner)) => {
                        format!("option<{}>", spell(resolve, inner))
                    }
                    (None, TypeDefKind::Result(result)) => format!(
                        "result<{}, {}>",
                        result
                            .ok
                            .map_or_else(|| "_".to_owned(), |ok| spell(resolve, &ok)),
                        result
                            .err
                            .map_or_else(|| "_".to_owned(), |err| spell(resolve, &err)),
                    ),
                    (None, other) => format!("{other:?}"),
                }
            }
            other => format!("{other:?}"),
        }
    }

    /// The parsed `wamn:node` package is exactly the ABI the router invokes:
    /// the handler signature, the node-context the router supplies, every
    /// node-error arm, and the emission. An exact comparison also keeps the
    /// retired frames, payload, credential and stream vocabulary out.
    #[test]
    fn node_abi_is_live_versioned_and_router_shaped() {
        let mut resolve = Resolve::new();
        let package = resolve
            .push_str("package.wit", NODE_WIT)
            .expect("the node ABI parses");
        let package = &resolve.packages[package];
        assert_eq!(package.name.to_string(), "wamn:node@0.1.0");
        assert_eq!(
            package.interfaces.keys().collect::<Vec<_>>(),
            ["types", "handler", "async-handler"]
        );
        assert_eq!(
            package.worlds.keys().collect::<Vec<_>>(),
            ["node", "async-node"]
        );

        let types = &resolve.interfaces[package.interfaces["types"]];
        assert_eq!(
            types.types.keys().collect::<Vec<_>>(),
            [
                "json",
                "node-context",
                "error-detail",
                "rate-limit-detail",
                "node-error",
                "emission"
            ]
        );
        let record = |name: &str| match &resolve.types[types.types[name]].kind {
            TypeDefKind::Record(record) => record
                .fields
                .iter()
                .map(|field| format!("{}: {}", field.name, spell(&resolve, &field.ty)))
                .collect::<Vec<_>>(),
            other => panic!("{name} must be a record, found {other:?}"),
        };
        // Occurrence keeps merge/loop visits distinct while retries retain one
        // visit identity; the wiring pair scopes it to the graph revision that
        // asked for the effect. The trace pair carries the only evidence, and
        // the deadline makes `cancelled` reachable without a cancellation import.
        // No operation (it selects the admitted digest) and no mutable delivery
        // context enter the handler.
        assert_eq!(
            record("node-context"),
            [
                "wiring-id: string",
                "wiring-version: u32",
                "node-id: string",
                "delivery-id: string",
                "input-port: option<string>",
                "occurrence: u32",
                "traceparent: option<string>",
                "tracestate: option<string>",
                "deadline-ms: option<u64>",
                "config: json",
            ]
        );
        assert_eq!(
            record("emission"),
            ["payload: json", "port: option<string>"]
        );

        // Every arm drives a different router action: retry, honour the source's
        // own delay, error edge, dead-letter, and "nothing failed".
        let TypeDefKind::Variant(error) = &resolve.types[types.types["node-error"]].kind else {
            panic!("node-error must be a variant");
        };
        assert_eq!(
            error
                .cases
                .iter()
                .map(|case| match &case.ty {
                    Some(ty) => format!("{}({})", case.name, spell(&resolve, ty)),
                    None => case.name.clone(),
                })
                .collect::<Vec<_>>(),
            [
                "retryable(error-detail)",
                "rate-limited(rate-limit-detail)",
                "terminal(error-detail)",
                "invalid-input(error-detail)",
                "cancelled",
            ]
        );

        let handler = &resolve.interfaces[package.interfaces["handler"]];
        assert_eq!(handler.functions.keys().collect::<Vec<_>>(), ["run"]);
        let run = &handler.functions["run"];
        assert_eq!(
            run.params
                .iter()
                .map(|param| format!("{}: {}", param.name, spell(&resolve, &param.ty)))
                .collect::<Vec<_>>(),
            ["ctx: node-context", "input: json"]
        );
        assert_eq!(
            run.result.map(|result| spell(&resolve, &result)).as_deref(),
            Some("result<emission, node-error>")
        );

        let node = &resolve.worlds[package.worlds["node"]];
        let exports = node
            .exports
            .values()
            .map(|item| match item {
                WorldItem::Interface { id, .. } => resolve.interfaces[*id].name.clone(),
                other => panic!("world node exports a non-interface item {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(exports, [Some("handler".to_owned())]);
    }
}
