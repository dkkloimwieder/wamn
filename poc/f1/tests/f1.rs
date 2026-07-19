//! wamn-f1 gates: exact-decimal arithmetic, payload validation (incl. the
//! no-float rule), spec evaluation (boundary = in-spec), inter-node shape
//! round-trips, SQL shape pins, and the two drift-guards — the SQL identifiers
//! against the poc-receiving catalog fixture, and the implemented node set
//! against the production flow graph `deploy/poc/f1-flow.json`.

use std::cmp::Ordering;

use serde_json::{Value, json};
use wamn_f1::{
    Decimal, EvalBranchOut, HoldEntry, LineSpec, NODE_TYPES, OutOfSpec, UpsertOut, ValidateOut,
    evaluate_line, ok_body, parse_receipt, sql,
};

fn valid_payload() -> Value {
    json!({
        "receipt_no": "r-1001",
        "supplier": "acme",
        "site": "hq",
        "received_at": "2026-07-12T08:00:00Z",
        "lines": [
            { "material": "resin-a", "quantity": "100.000",
              "moisture_pct": "11.20", "weight_kg": "99.980" }
        ]
    })
}

// ---------------------------------------------------------------- decimal

#[test]
fn decimal_parses_canonical_forms_and_rejects_the_rest() {
    for ok in ["0", "12", "12.50", "-0.5", "0.050", "9007199254740993"] {
        assert!(Decimal::parse(ok).is_ok(), "{ok:?} should parse");
    }
    for bad in [
        "", ".", ".5", "12.", "1e5", "NaN", "+1", "1 2", "12.5.0", "0x1f",
    ] {
        assert!(Decimal::parse(bad).is_err(), "{bad:?} should be rejected");
    }
}

#[test]
fn decimal_comparison_is_scale_independent_and_exact() {
    let a = Decimal::parse("12.50").unwrap();
    let b = Decimal::parse("12.5").unwrap();
    assert_eq!(a.cmp_value(&b), Ordering::Equal);
    // The classic float trap: 0.1 + 0.2 style artifacts cannot appear — the
    // deviation of 100.000 vs 99.950 is exactly the 0.050 tolerance.
    let dev = Decimal::parse("100.000")
        .unwrap()
        .abs_diff(&Decimal::parse("99.950").unwrap());
    assert_eq!(dev.to_string(), "0.050");
    assert_eq!(
        dev.cmp_value(&Decimal::parse("0.050").unwrap()),
        Ordering::Equal
    );
}

#[test]
fn decimal_fits_mirrors_numeric_precision_scale() {
    // numeric(5,2): up to 999.99; leading zeros do not count as int digits.
    let fits = |s: &str| Decimal::parse(s).unwrap().fits(5, 2);
    assert!(fits("999.99") && fits("0.50") && fits("123"));
    assert!(!fits("1000") && !fits("1.234"));
}

// ---------------------------------------------------------------- payload

#[test]
fn valid_receipt_parses_with_values_verbatim() {
    let r = parse_receipt(&valid_payload()).expect("valid payload");
    assert_eq!(r.receipt_no, "r-1001");
    assert_eq!(r.lines.len(), 1);
    // Values stay verbatim — no canonicalization.
    assert_eq!(r.lines[0].quantity, "100.000");
}

#[test]
fn json_integer_decimals_are_accepted() {
    let mut v = valid_payload();
    v["lines"][0]["quantity"] = json!(100);
    let r = parse_receipt(&v).expect("integer quantity is exact");
    assert_eq!(r.lines[0].quantity, "100");
}

#[test]
fn payload_negatives_each_produce_an_issue() {
    let cases: Vec<(Value, &str)> = vec![
        (json!("not an object"), "payload must be a JSON object"),
        (
            {
                let mut v = valid_payload();
                v.as_object_mut().unwrap().remove("receipt_no");
                v
            },
            "$.receipt_no",
        ),
        (
            {
                let mut v = valid_payload();
                // A JSON float is refused outright — the no-float rule.
                v["lines"][0]["moisture_pct"] = json!(11.2);
                v
            },
            "no float",
        ),
        (
            {
                let mut v = valid_payload();
                v["lines"][0]["quantity"] = json!("12.5.0");
                v
            },
            "not a decimal",
        ),
        (
            {
                let mut v = valid_payload();
                v["lines"][0]["quantity"] = json!("-3.000");
                v
            },
            "must be positive",
        ),
        (
            {
                let mut v = valid_payload();
                v["lines"][0]["moisture_pct"] = json!("1234.5");
                v
            },
            "out of range for numeric(5,2)",
        ),
        (
            {
                let mut v = valid_payload();
                v["received_at"] = json!("last tuesday");
                v
            },
            "RFC 3339",
        ),
        (
            {
                let mut v = valid_payload();
                v["lines"] = json!([]);
                v
            },
            "must not be empty",
        ),
        (
            {
                let mut v = valid_payload();
                v["receipt_no"] = json!("x".repeat(65));
                v
            },
            "longer than 64",
        ),
        (
            {
                let mut v = valid_payload();
                v["surprise"] = json!(1);
                v
            },
            "unknown key",
        ),
    ];
    for (payload, expect) in cases {
        let issues = parse_receipt(&payload).expect_err("must be rejected");
        let all = issues
            .iter()
            .map(|i| format!("{}: {}", i.path, i.message))
            .collect::<Vec<_>>()
            .join("; ");
        assert!(
            all.contains(expect)
                || (expect == "no float" && all.contains("JSON floats are not accepted")),
            "expected {expect:?} in issues, got: {all}"
        );
    }
}

#[test]
fn all_violations_are_reported_at_once() {
    let issues = parse_receipt(&json!({
        "supplier": "acme",
        "lines": [{ "material": "resin-a", "quantity": 1.5,
                    "moisture_pct": "1.00", "weight_kg": "1.000" }]
    }))
    .expect_err("multiple violations");
    // receipt_no + site + received_at missing, plus the float quantity.
    assert!(issues.len() >= 4, "got {issues:?}");
}

// ---------------------------------------------------------------- evaluate

#[test]
fn boundary_equality_is_in_spec() {
    // Exactly at the moisture max AND exactly at the weight tolerance: in-spec.
    let reasons = evaluate_line("100.000", "12.50", "99.950", "12.5", "0.050").unwrap();
    assert!(reasons.is_empty(), "boundary must be in-spec: {reasons:?}");
}

#[test]
fn strict_exceedance_is_out_of_spec_with_reasons() {
    let reasons = evaluate_line("100.000", "12.51", "99.949", "12.50", "0.050").unwrap();
    assert_eq!(reasons.len(), 2, "{reasons:?}");
    assert!(reasons[0].contains("moisture 12.51 pct exceeds max 12.50 pct"));
    assert!(reasons[1].contains("deviates 0.051 kg"));
    assert!(reasons[1].contains("tolerance 0.050 kg"));
}

#[test]
fn weight_deviation_is_symmetric() {
    // Overweight and underweight both count.
    for weight in ["100.060", "99.940"] {
        let reasons = evaluate_line("100.000", "0.00", weight, "12.50", "0.050").unwrap();
        assert_eq!(reasons.len(), 1, "{weight}: {reasons:?}");
    }
}

// ---------------------------------------------------------------- shapes

#[test]
fn inter_node_shapes_survive_a_json_round_trip() {
    let receipt = parse_receipt(&valid_payload()).unwrap();
    let validate = ValidateOut {
        receipt: receipt.clone(),
        supplier_id: "11111111-1111-1111-1111-111111111111".into(),
        site_id: "22222222-2222-2222-2222-222222222222".into(),
        line_specs: vec![LineSpec {
            material_id: "33333333-3333-3333-3333-333333333333".into(),
            moisture_max_pct: "12.50".into(),
            weight_tolerance_kg: "0.050".into(),
        }],
    };
    assert_eq!(
        ValidateOut::from_value(&validate.to_value()).unwrap(),
        validate
    );

    let upsert = UpsertOut {
        receipt,
        supplier_id: validate.supplier_id.clone(),
        site_id: validate.site_id.clone(),
        line_specs: validate.line_specs.clone(),
        receipt_id: "44444444-4444-4444-4444-444444444444".into(),
        line_ids: vec!["55555555-5555-5555-5555-555555555555".into()],
    };
    assert_eq!(UpsertOut::from_value(&upsert.to_value()).unwrap(), upsert);

    let branch = EvalBranchOut {
        receipt_id: upsert.receipt_id.clone(),
        site_id: upsert.site_id.clone(),
        out_of_spec: vec![OutOfSpec {
            line: 1,
            line_id: upsert.line_ids[0].clone(),
            material: "resin-a".into(),
            reason: "moisture 13.10 pct exceeds max 12.50 pct".into(),
        }],
    };
    assert_eq!(
        EvalBranchOut::from_value(&branch.to_value()).unwrap(),
        branch
    );
}

#[test]
fn respond_status_answers_configured_or_503_for_foreign_errors() {
    use wamn_f1::respond_status;
    // Plain success respond: configured status; default 200.
    assert_eq!(
        respond_status(&json!({"status": 200}), &json!({"x": 1})),
        200
    );
    assert_eq!(respond_status(&json!({}), &json!({})), 200);
    // Error-path respond answering for the code it was configured for: 400.
    let cfg = json!({"status": 400, "error": "invalid-input"});
    let client_fault = json!({"error": {"code": "invalid-input", "message": "m"}});
    assert_eq!(respond_status(&cfg, &client_fault), 400);
    // A DIFFERENT error routed down the error edge — the platform's fault, a
    // DB outage during validate's resolution — must not surface as a 400.
    let infra_fault = json!({"error": {"code": "connection-unavailable"}});
    assert_eq!(respond_status(&cfg, &infra_fault), 503);
    // An error payload with no code at all is also not the configured fault.
    assert_eq!(
        respond_status(&cfg, &json!({"error": {"message": "m"}})),
        503
    );
}

#[test]
fn response_body_matches_the_acceptance_contract() {
    let body = ok_body(
        "44444444-4444-4444-4444-444444444444",
        &[HoldEntry {
            hold_id: "66666666-6666-6666-6666-666666666666".into(),
            line: 1,
            material: "resin-a".into(),
            reason: "moisture 13.10 pct exceeds max 12.50 pct".into(),
            status: "open".into(),
        }],
    );
    assert_eq!(body["receipt_id"], "44444444-4444-4444-4444-444444444444");
    assert_eq!(body["holds"][0]["status"], "open");
    assert_eq!(body["holds"][0]["line"], 1);
    let empty = ok_body("x", &[]);
    assert_eq!(empty["holds"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------- sql shape

#[test]
fn sql_is_parameterized_and_unqualified() {
    let all = [
        sql::RESOLVE_SUPPLIER,
        sql::RESOLVE_SITE,
        sql::RESOLVE_MATERIAL,
        sql::UPSERT_RECEIPT,
        sql::DELETE_LINES,
        sql::INSERT_LINE,
        sql::INSERT_HOLD,
    ];
    for s in all {
        // No schema qualification (search_path resolves) and no format-string
        // remnants; every runtime value travels as a $n parameter.
        assert!(!s.contains('{') && !s.contains('}'), "{s}");
        assert!(!s.contains("wamn_run."), "{s}");
        assert!(s.contains("$1"), "{s}");
    }
    // Writes are tenant-stamped server-side and return the generated id.
    for s in [sql::UPSERT_RECEIPT, sql::INSERT_LINE, sql::INSERT_HOLD] {
        assert!(s.contains("current_setting('app.tenant', true)"), "{s}");
        assert!(s.contains("RETURNING id::text"), "{s}");
    }
    // The upsert conflicts on the tenant-scoped composite natural key.
    assert!(
        sql::UPSERT_RECEIPT.contains("ON CONFLICT (tenant_id, receipt_no, supplier_id)"),
        "upsert must target the receipts_no_supplier_uniq columns"
    );
    // Holds are born open, opened server-side.
    assert!(sql::INSERT_HOLD.contains("'open'") && sql::INSERT_HOLD.contains("now()"));
}

// ---------------------------------------------------------------- drift guards

/// The SQL identifiers and numeric domains this crate hardcodes must match the
/// poc-receiving catalog fixture the schema is generated from.
#[test]
fn catalog_drift_guard() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/wamn-catalog/tests/fixtures/poc-receiving.catalog.json"
    ))
    .expect("read poc-receiving catalog fixture");
    let cat: Value = serde_json::from_str(&src).unwrap();
    let entity = |id: &str| {
        cat["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .unwrap_or_else(|| panic!("entity {id}"))
            .clone()
    };
    let field = |e: &Value, id: &str| {
        e["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == id)
            .unwrap_or_else(|| panic!("field {id}"))
            .clone()
    };

    // Numeric domains the payload validator enforces.
    let materials = entity("materials");
    let moisture = field(&materials, "moisture_max_pct");
    assert_eq!(moisture["type"]["precision"], 5);
    assert_eq!(moisture["type"]["scale"], 2);
    let tolerance = field(&materials, "weight_tolerance_kg");
    assert_eq!(tolerance["type"]["precision"], 8);
    assert_eq!(tolerance["type"]["scale"], 3);
    let lines = entity("receipt_lines");
    let quantity = field(&lines, "quantity");
    assert_eq!(quantity["type"]["precision"], 12);
    assert_eq!(quantity["type"]["scale"], 3);

    // receipt_no length cap + the composite unique the upsert targets.
    let receipts = entity("receipts");
    assert_eq!(field(&receipts, "receipt_no")["type"]["max-len"], 64);
    let uniq = &receipts["constraints"][0];
    assert_eq!(uniq["fields"], json!(["receipt_no", "supplier_id"]));

    // Column names the SQL references exist as catalog field names.
    for (entity_id, fields) in [
        ("suppliers", vec!["name"]),
        ("sites", vec!["name", "code"]),
        (
            "materials",
            vec!["name", "moisture_max_pct", "weight_tolerance_kg"],
        ),
        (
            "receipts",
            vec!["receipt_no", "supplier_id", "site_id", "received_at"],
        ),
        (
            "receipt_lines",
            vec!["receipt_id", "material_id", "quantity"],
        ),
        (
            "quality_holds",
            vec!["line_id", "site_id", "status", "opened_at"],
        ),
    ] {
        let e = entity(entity_id);
        for f in fields {
            field(&e, f); // panics on drift
        }
    }
    // Holds are created 'open' — the enum must carry the variant.
    let holds = entity("quality_holds");
    let status = field(&holds, "status");
    assert!(
        status["type"]["variants"]
            .as_array()
            .unwrap()
            .contains(&json!("open"))
    );
}

/// The production flow graph must validate under wamn-flow and reference only
/// the node types this crate implements, with the F1 topology (sync webhook on
/// /receipts, validate error path, evaluate out-of-spec branch).
#[test]
fn flow_drift_guard() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/poc/f1-flow.json"
    ))
    .expect("read deploy/poc/f1-flow.json");
    let flow = wamn_flow::Flow::from_json(&src).expect("parse f1 flow");
    assert!(
        flow.issues().is_empty(),
        "flow must validate clean: {:?}",
        flow.issues()
    );
    assert_eq!(flow.flow_id, "receipt-received");
    assert!(matches!(
        &flow.trigger,
        wamn_flow::Trigger::Webhook { sync: true, path: Some(p) } if p == "/receipts"
    ));
    for node in &flow.nodes {
        assert!(
            NODE_TYPES.contains(&node.node_type.as_str()),
            "unimplemented node type {:?}",
            node.node_type
        );
    }
    let has_edge = |from: &str, port: &str, to: &str| {
        flow.edges
            .iter()
            .any(|e| e.from == from && e.from_port == port && e.to == to)
    };
    assert!(has_edge("validate", "error", "respond-bad"));
    assert!(has_edge("evaluate", "out-of-spec", "holds"));
    assert!(has_edge("evaluate", "main", "respond-ok"));
    assert!(has_edge("holds", "main", "respond-ok"));
}
