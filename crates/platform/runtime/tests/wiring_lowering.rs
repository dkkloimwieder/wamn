use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use wamn_catalog::{WiringDocument, WiringEdge, WiringNode, WiringTerminal};
use wamn_router::{
    DEDUP_ID_FIELD, Delivery, NodeCall, NodeOutcome, Step, Terminal, Verdict, WalkStatus, Wiring,
};
use wamn_runtime::wiring_lowering::{
    GatedActiveWiring, ScopedWiringOperationFacts, WiringLoweringErrorKind, WiringOperationFact,
    WiringParameterFact, WiringScope, lower_active_wiring,
};

const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn scope(environment: &'static str) -> WiringScope<'static> {
    WiringScope {
        tenant_id: "tenant-a",
        catalog_id: "catalog-a",
        environment,
    }
}

fn operation(
    component: &str,
    interface_version: &str,
    operation: &str,
    digest: &str,
    input_ports: &[&str],
    output_ports: &[&str],
    parameters: &[(&str, bool)],
) -> WiringOperationFact {
    WiringOperationFact {
        component: component.to_owned(),
        interface_version: interface_version.to_owned(),
        operation: operation.to_owned(),
        component_digest: digest.to_owned(),
        input_ports: input_ports.iter().map(|port| (*port).to_owned()).collect(),
        output_ports: output_ports.iter().map(|port| (*port).to_owned()).collect(),
        parameters: parameters
            .iter()
            .map(|(name, required)| {
                (
                    (*name).to_owned(),
                    WiringParameterFact {
                        required: *required,
                    },
                )
            })
            .collect(),
    }
}

fn operations() -> Vec<WiringOperationFact> {
    vec![
        operation(
            "parser",
            "0.1.0",
            "parse",
            DIGEST_A,
            &["payload"],
            &["record"],
            &[("format", true)],
        ),
        operation(
            "sink",
            "0.2.0",
            "write",
            DIGEST_B,
            &["record"],
            &["main"],
            &[("relation", true), ("audit", false)],
        ),
    ]
}

fn document(to_port: Option<&str>) -> WiringDocument {
    WiringDocument::new(
        "orders",
        3,
        "decode",
        BTreeMap::from([
            (
                "audit".to_owned(),
                WiringNode {
                    component: "sink".to_owned(),
                    interface_version: "0.2.0".to_owned(),
                    operation: "write".to_owned(),
                    params: BTreeMap::from([("relation".to_owned(), json!("order-audit"))]),
                    terminal: Some(WiringTerminal::Emit),
                },
            ),
            (
                "decode".to_owned(),
                WiringNode {
                    component: "parser".to_owned(),
                    interface_version: "0.1.0".to_owned(),
                    operation: "parse".to_owned(),
                    params: BTreeMap::from([("format".to_owned(), json!("csv"))]),
                    terminal: None,
                },
            ),
            (
                "persist".to_owned(),
                WiringNode {
                    component: "sink".to_owned(),
                    interface_version: "0.2.0".to_owned(),
                    operation: "write".to_owned(),
                    params: BTreeMap::from([("relation".to_owned(), json!("orders"))]),
                    terminal: Some(WiringTerminal::Respond),
                },
            ),
        ]),
        vec![
            WiringEdge {
                from: "decode".to_owned(),
                from_port: "record".to_owned(),
                to: "persist".to_owned(),
                to_port: to_port.map(str::to_owned),
            },
            WiringEdge {
                from: "decode".to_owned(),
                from_port: "record".to_owned(),
                to: "audit".to_owned(),
                to_port: to_port.map(str::to_owned),
            },
        ],
        Vec::new(),
    )
    .expect("fixture wiring is structurally valid")
}

fn lower<'a>(
    document: &'a WiringDocument,
    operations: &'a [WiringOperationFact],
) -> Result<Wiring, wamn_runtime::wiring_lowering::WiringLoweringError> {
    lower_active_wiring(
        GatedActiveWiring {
            scope: scope("prod"),
            gated_catalog_version: 7,
            document,
        },
        ScopedWiringOperationFacts {
            scope: scope("prod"),
            catalog_version: 7,
            operations,
        },
    )
}

fn first_routed_call(wiring: &Wiring) -> NodeCall {
    let mut walk = wiring.start(Delivery {
        id: "delivery-a".to_owned(),
        payload: json!({"raw": "record"}),
        caller_attached: true,
    });
    let Step::Invoke(entry) = wiring.next(&mut walk, 0) else {
        panic!("entry invocation is ready");
    };
    assert_eq!(entry.input_port, None, "the entry has no incoming edge");
    wiring
        .apply(
            &mut walk,
            &entry,
            NodeOutcome::ok_on(json!({"id": 7}), "record"),
            0,
        )
        .expect("entry output advances to its selected successor");
    let Step::Invoke(target) = wiring.next(&mut walk, 0) else {
        panic!("first routed target is ready");
    };
    target
}

fn stored_single_node_document(terminal: Option<&str>) -> WiringDocument {
    WiringDocument::parse(&json!({
        "format-version": "0.1",
        "wiring-id": "stored-terminal",
        "version": 1,
        "entry": "entry",
        "nodes": {
            "entry": {
                "component": "parser",
                "interface-version": "0.1.0",
                "operation": "parse",
                "params": { "format": "csv" },
                "terminal": terminal,
            },
        },
    }))
    .expect("stored wiring document is valid")
}

fn drive_stored_single_node(
    document: &WiringDocument,
    caller_attached: bool,
    output: Value,
) -> (WalkStatus, Option<Verdict>) {
    let wiring = lower(document, &operations()).expect("stored wiring lowers");
    let mut walk = wiring.start(Delivery {
        id: "stored-delivery".to_owned(),
        payload: json!({"input": "value"}),
        caller_attached,
    });
    let mut invocations = 0;

    loop {
        match wiring.next(&mut walk, 0) {
            Step::Invoke(call) => {
                invocations += 1;
                assert_eq!(call.node, "entry");
                wiring
                    .apply(&mut walk, &call, NodeOutcome::ok(output.clone()), 0)
                    .expect("stored wiring outcome applies");
            }
            Step::Wait { .. } => panic!("single-node stored wiring cannot wait"),
            Step::Done(status) => {
                assert_eq!(invocations, 1, "the stored entry runs exactly once");
                return (status, walk.verdict().cloned());
            }
        }
    }
}

#[test]
fn stored_respond_terminal_reaches_the_router_verdict() {
    let document = stored_single_node_document(Some("respond"));
    let output = json!({"answer": 42});

    let (status, verdict) = drive_stored_single_node(&document, true, output.clone());

    assert_eq!(status, WalkStatus::Completed);
    assert_eq!(
        verdict,
        Some(Verdict::Respond {
            payload: output,
            node_id: "entry".to_owned(),
        })
    );
}

#[test]
fn stored_emit_terminal_reaches_the_router_verdict() {
    let document = stored_single_node_document(Some("emit"));
    let output = json!({DEDUP_ID_FIELD: "stored-terminal:1:entry", "order": 42});

    let (status, verdict) = drive_stored_single_node(&document, false, output.clone());

    assert_eq!(status, WalkStatus::Completed);
    assert_eq!(
        verdict,
        Some(Verdict::Emit {
            event: output,
            dedup_id: "stored-terminal:1:entry".to_owned(),
        })
    );
}

#[test]
fn stored_non_terminal_reaches_the_router_discard_verdict() {
    let document = stored_single_node_document(None);

    let (status, verdict) = drive_stored_single_node(&document, false, json!({"handled": true}));

    assert_eq!(status, WalkStatus::Completed);
    assert_eq!(verdict, Some(Verdict::Discard));
}

#[test]
fn lowers_names_to_digest_keyed_nodes_and_preserves_config_terminal_and_order() {
    let document = document(None);
    let operations = operations();
    let wiring = lower(&document, &operations).expect("single target input is inferred");

    assert_eq!(wiring.entry(), "decode");
    let decode = wiring.node("decode").expect("decode node is lowered");
    assert_eq!(decode.component, DIGEST_A);
    assert_eq!(decode.config, json!({"format": "csv"}));
    assert_eq!(decode.terminal, None);
    let persist = wiring.node("persist").expect("persist node is lowered");
    assert_eq!(persist.component, DIGEST_B);
    assert_eq!(persist.config, json!({"relation": "orders"}));
    assert_eq!(persist.terminal, Some(Terminal::Respond));
    let audit = wiring.node("audit").expect("audit node is lowered");
    assert_eq!(audit.component, DIGEST_B);
    assert_eq!(audit.config, json!({"relation": "order-audit"}));
    assert_eq!(audit.terminal, Some(Terminal::Emit));

    let edges: Vec<_> = wiring.successors("decode", "record").collect();
    assert_eq!(
        edges
            .iter()
            .map(|edge| edge.to.as_str())
            .collect::<Vec<_>>(),
        ["persist", "audit"]
    );
    assert!(edges.iter().all(|edge| edge.to_port == "record"));
    assert_eq!(
        first_routed_call(&wiring).input_port.as_deref(),
        Some("record"),
        "the inferred singleton input reaches the invocation boundary"
    );
}

#[test]
fn explicit_target_port_is_required_only_when_the_target_has_multiple_inputs() {
    let mut operations = operations();
    operations[1].input_ports = BTreeSet::from(["batch".to_owned(), "record".to_owned()]);

    let explicit = document(Some("record"));
    let explicit =
        lower(&explicit, &operations).expect("an explicit declared input is unambiguous");
    assert_eq!(
        first_routed_call(&explicit).input_port.as_deref(),
        Some("record"),
        "the explicit input reaches the invocation boundary"
    );

    let omitted = document(None);
    assert_eq!(
        lower(&omitted, &operations).unwrap_err().kind(),
        WiringLoweringErrorKind::AmbiguousInputPort
    );

    let unknown = document(Some("missing"));
    assert_eq!(
        lower(&unknown, &operations).unwrap_err().kind(),
        WiringLoweringErrorKind::UnknownInputPort
    );
}

#[test]
fn component_interface_operation_and_port_drift_each_refuse() {
    let document = document(None);

    let mut missing_component = operations();
    missing_component[0].component = "other".to_owned();
    assert_eq!(
        lower(&document, &missing_component).unwrap_err().kind(),
        WiringLoweringErrorKind::MissingComponent
    );

    let mut interface_drift = operations();
    interface_drift[0].interface_version = "0.2.0".to_owned();
    assert_eq!(
        lower(&document, &interface_drift).unwrap_err().kind(),
        WiringLoweringErrorKind::IncompatibleInterfaceVersion
    );

    let mut operation_drift = operations();
    operation_drift[0].operation = "decode".to_owned();
    assert_eq!(
        lower(&document, &operation_drift).unwrap_err().kind(),
        WiringLoweringErrorKind::MissingOperation
    );

    let mut output_drift = operations();
    output_drift[0].output_ports = BTreeSet::from(["main".to_owned()]);
    assert_eq!(
        lower(&document, &output_drift).unwrap_err().kind(),
        WiringLoweringErrorKind::UnknownOutputPort
    );
}

#[test]
fn parameter_and_scope_facts_cannot_drift_one_side_of_the_boundary() {
    let document = document(None);

    let mut undeclared = operations();
    undeclared[0].parameters.clear();
    assert_eq!(
        lower(&document, &undeclared).unwrap_err().kind(),
        WiringLoweringErrorKind::UndeclaredParameter
    );

    let mut required = operations();
    required[0].parameters.insert(
        "delimiter".to_owned(),
        WiringParameterFact { required: true },
    );
    assert_eq!(
        lower(&document, &required).unwrap_err().kind(),
        WiringLoweringErrorKind::MissingRequiredParameter
    );

    let operations = operations();
    let environment_drift = lower_active_wiring(
        GatedActiveWiring {
            scope: scope("prod"),
            gated_catalog_version: 7,
            document: &document,
        },
        ScopedWiringOperationFacts {
            scope: scope("stage"),
            catalog_version: 7,
            operations: &operations,
        },
    )
    .unwrap_err();
    assert_eq!(
        environment_drift.kind(),
        WiringLoweringErrorKind::ScopeMismatch
    );

    let version_drift = lower_active_wiring(
        GatedActiveWiring {
            scope: scope("prod"),
            gated_catalog_version: 7,
            document: &document,
        },
        ScopedWiringOperationFacts {
            scope: scope("prod"),
            catalog_version: 8,
            operations: &operations,
        },
    )
    .unwrap_err();
    assert_eq!(
        version_drift.kind(),
        WiringLoweringErrorKind::CatalogVersionMismatch
    );
}
