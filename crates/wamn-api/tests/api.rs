//! Integration tests for the REST gateway compiler over the POC catalog.
//!
//! These assert the *emitted SQL + params* (deterministic, no DB): CRUD shapes,
//! filter/sort/paginate/expand, and — the S2 stop-the-line concern — that every
//! user value stays a `$n` parameter and every identifier is catalog-allowlisted
//! (unknown → a typed 4xx error, never SQL).

use serde_json::{Value, json};
use wamn_api::{
    ApiError, ExpandDir, Method, PlanKind, Router, SqlValue, attach_expansion, shape_rows,
};
use wamn_catalog::Catalog;

const POC: &str = include_str!("../../wamn-catalog/tests/fixtures/poc-receiving.catalog.json");

fn catalog() -> Catalog {
    Catalog::from_json(POC).expect("POC fixture parses")
}

fn q(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

const A_UUID: &str = "11111111-1111-1111-1111-111111111111";
const B_UUID: &str = "22222222-2222-2222-2222-222222222222";

// ---- routing / CRUD shapes ------------------------------------------------

#[test]
fn list_projects_id_and_fields_but_not_tenant_id() {
    let cat = catalog();
    let plan = Router::new(&cat)
        .compile(Method::Get, "/api/rest/suppliers", &[], None)
        .unwrap();
    assert_eq!(plan.kind, PlanKind::List);
    // id + the three user fields, in order; tenant_id absent.
    assert!(plan.query.sql.starts_with(
        "SELECT \"id\", \"name\", \"contact_email\", \"standard_cost\" FROM \"suppliers\""
    ));
    assert!(!plan.query.sql.contains("tenant_id"));
    // default order + a capped page.
    assert!(plan.query.sql.contains("ORDER BY \"id\" ASC"));
    assert!(plan.query.sql.contains("LIMIT $1 OFFSET $2"));
    assert_eq!(
        plan.query.params,
        vec![SqlValue::Int64(50), SqlValue::Int64(0)]
    );
    assert_eq!(
        plan.query.columns,
        vec!["id", "name", "contact_email", "standard_cost"]
    );
}

#[test]
fn get_by_id_binds_uuid_param() {
    let cat = catalog();
    let path = format!("/api/rest/suppliers/{A_UUID}");
    let plan = Router::new(&cat)
        .compile(Method::Get, &path, &[], None)
        .unwrap();
    assert_eq!(plan.kind, PlanKind::GetOne);
    assert_eq!(
        plan.query.sql,
        "SELECT \"id\", \"name\", \"contact_email\", \"standard_cost\" FROM \"suppliers\" WHERE \"id\" = $1"
    );
    assert_eq!(plan.query.params, vec![SqlValue::Uuid(A_UUID.to_string())]);
}

#[test]
fn create_sets_tenant_from_claim_and_returns_projection() {
    let cat = catalog();
    let body = json!({ "name": "Acme", "standard_cost": "12.50" });
    let plan = Router::new(&cat)
        .compile(Method::Post, "/api/rest/suppliers", &[], Some(&body))
        .unwrap();
    assert_eq!(plan.kind, PlanKind::CreateOne);
    assert_eq!(plan.status, 201);
    // tenant_id set server-side from the claim (not a param), user cols bound.
    assert_eq!(
        plan.query.sql,
        "INSERT INTO \"suppliers\" (\"tenant_id\", \"name\", \"standard_cost\") \
         VALUES (current_setting('app.tenant', true), $1, $2) \
         RETURNING \"id\", \"name\", \"contact_email\", \"standard_cost\""
    );
    assert_eq!(
        plan.query.params,
        vec![
            SqlValue::Text("Acme".into()),
            SqlValue::Numeric("12.50".into())
        ]
    );
}

#[test]
fn create_rejects_missing_required_field() {
    let cat = catalog();
    // suppliers.name is NOT NULL with no default → required.
    let body = json!({ "contact_email": "x@y.z" });
    let err = Router::new(&cat)
        .compile(Method::Post, "/api/rest/suppliers", &[], Some(&body))
        .unwrap_err();
    assert!(
        matches!(err, ApiError::InvalidValue { ref field, .. } if field == "name"),
        "{err:?}"
    );
    assert_eq!(err.status(), 400);
}

#[test]
fn update_is_partial_and_binds_id_last() {
    let cat = catalog();
    let path = format!("/api/rest/suppliers/{A_UUID}");
    let body = json!({ "contact_email": "new@acme.test" });
    let plan = Router::new(&cat)
        .compile(Method::Patch, &path, &[], Some(&body))
        .unwrap();
    assert_eq!(plan.kind, PlanKind::UpdateOne);
    assert_eq!(
        plan.query.sql,
        "UPDATE \"suppliers\" SET \"contact_email\" = $1 WHERE \"id\" = $2 \
         RETURNING \"id\", \"name\", \"contact_email\", \"standard_cost\""
    );
    assert_eq!(
        plan.query.params,
        vec![
            SqlValue::Text("new@acme.test".into()),
            SqlValue::Uuid(A_UUID.to_string())
        ]
    );
}

#[test]
fn delete_returns_id_and_204() {
    let cat = catalog();
    let path = format!("/api/rest/suppliers/{A_UUID}");
    let plan = Router::new(&cat)
        .compile(Method::Delete, &path, &[], None)
        .unwrap();
    assert_eq!(plan.kind, PlanKind::DeleteOne);
    assert_eq!(plan.status, 204);
    assert_eq!(
        plan.query.sql,
        "DELETE FROM \"suppliers\" WHERE \"id\" = $1 RETURNING \"id\""
    );
    assert_eq!(plan.query.params, vec![SqlValue::Uuid(A_UUID.to_string())]);
}

// ---- filter / sort / paginate ---------------------------------------------

#[test]
fn filter_operators_and_typed_values() {
    let cat = catalog();
    // status=eq.open (enum), site filter reserved words don't clash.
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/quality_holds",
            &q(&[("status", "eq.open")]),
            None,
        )
        .unwrap();
    assert!(
        plan.query.sql.contains("WHERE \"status\" = $1"),
        "{}",
        plan.query.sql
    );
    assert_eq!(plan.query.params[0], SqlValue::Text("open".into()));
}

#[test]
fn bare_value_is_eq_even_with_a_dot() {
    let cat = catalog();
    // "12.50" must be eq to the literal, NOT parsed as operator "12".
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/suppliers",
            &q(&[("standard_cost", "12.50")]),
            None,
        )
        .unwrap();
    assert!(plan.query.sql.contains("\"standard_cost\" = $1"));
    assert_eq!(plan.query.params[0], SqlValue::Numeric("12.50".into()));
}

#[test]
fn in_list_binds_one_param_per_value() {
    let cat = catalog();
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/quality_holds",
            &q(&[("status", "in.open,escalated")]),
            None,
        )
        .unwrap();
    assert!(
        plan.query.sql.contains("\"status\" IN ($1, $2)"),
        "{}",
        plan.query.sql
    );
    assert_eq!(plan.query.params[0], SqlValue::Text("open".into()));
    assert_eq!(plan.query.params[1], SqlValue::Text("escalated".into()));
}

#[test]
fn sort_and_paginate_are_capped_and_parametrized() {
    let cat = catalog();
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/receipts",
            &q(&[
                ("sort", "received_at,-receipt_no"),
                ("limit", "9999"),
                ("offset", "40"),
            ]),
            None,
        )
        .unwrap();
    assert!(
        plan.query
            .sql
            .contains("ORDER BY \"received_at\" ASC, \"receipt_no\" DESC")
    );
    // limit clamped to the max page size (100), offset passed through, both params.
    let n = plan.query.params.len();
    assert_eq!(plan.query.params[n - 2], SqlValue::Int64(100));
    assert_eq!(plan.query.params[n - 1], SqlValue::Int64(40));
}

#[test]
fn out_of_range_offset_is_rejected_not_wrapped() {
    let cat = catalog();
    // >= 2^63 would wrap to a negative i64 OFFSET the DB refuses; reject cleanly.
    let err = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/suppliers",
            &q(&[("offset", "9223372036854775808")]),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, ApiError::InvalidRequest(_)), "{err:?}");
    assert_eq!(err.status(), 400);
}

// ---- expansion ------------------------------------------------------------

#[test]
fn expand_to_one_parent_via_fk() {
    let cat = catalog();
    // quality_holds --(line_id)--> receipt_lines : the resource holds the FK.
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/quality_holds",
            &q(&[("expand", "line")]),
            None,
        )
        .unwrap();
    assert_eq!(plan.expands.len(), 1);
    let ex = &plan.expands[0];
    assert_eq!(ex.dir, ExpandDir::ToOne);
    assert_eq!(ex.key_column, "line_id");
    assert_eq!(ex.target_table, "receipt_lines");
    assert_eq!(ex.match_column, "id");

    let keys = vec![SqlValue::Uuid(A_UUID.into()), SqlValue::Uuid(B_UUID.into())];
    let sub = Router::new(&cat).build_expand(ex, &keys);
    assert_eq!(
        sub.sql,
        "SELECT \"id\", \"receipt_id\", \"material_id\", \"quantity\" FROM \"receipt_lines\" WHERE \"id\" IN ($1, $2)"
    );
    assert_eq!(sub.params, keys);
}

#[test]
fn expand_to_many_children_via_reverse_fk() {
    let cat = catalog();
    // receipts is the parent of receipt_lines (relation "lines", from=receipt_lines to=receipts).
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/receipts",
            &q(&[("expand", "lines")]),
            None,
        )
        .unwrap();
    let ex = &plan.expands[0];
    assert_eq!(ex.dir, ExpandDir::ToMany);
    assert_eq!(ex.key_column, "id");
    assert_eq!(ex.target_table, "receipt_lines");
    assert_eq!(ex.match_column, "receipt_id");
}

#[test]
fn expand_merge_embeds_records() {
    let cat = catalog();
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/quality_holds",
            &q(&[("expand", "line")]),
            None,
        )
        .unwrap();
    let ex = &plan.expands[0];

    // Two holds, both pointing at line A.
    let mut primary = shape_rows(
        &plan.query.columns,
        &[vec![
            SqlValue::Uuid("hold-1".into()),
            SqlValue::Uuid(A_UUID.into()),
            SqlValue::Uuid("site-1".into()),
            SqlValue::Text("open".into()),
            SqlValue::Timestamptz("t".into()),
        ]],
    );
    let expanded_rows = vec![vec![
        SqlValue::Uuid(A_UUID.into()),
        SqlValue::Uuid("receipt-1".into()),
        SqlValue::Uuid("mat-1".into()),
        SqlValue::Numeric("3.000".into()),
    ]];
    attach_expansion(&mut primary, ex, &ex.columns, &expanded_rows);
    let line = &primary[0]["line"];
    assert_eq!(line["id"], json!(A_UUID));
    assert_eq!(line["quantity"], json!("3.000")); // exact-decimal string, not a float
}

// ---- exact-decimal round-trip (no float, end to end) ----------------------

#[test]
fn numeric_stays_an_exact_decimal_string_in_and_out() {
    let cat = catalog();
    // In: a body decimal string is bound as Numeric (not a float).
    let body = json!({ "name": "M", "moisture_max_pct": "12.34", "weight_tolerance_kg": "0.500" });
    let plan = Router::new(&cat)
        .compile(Method::Post, "/api/rest/materials", &[], Some(&body))
        .unwrap();
    assert!(
        plan.query
            .params
            .contains(&SqlValue::Numeric("12.34".into()))
    );
    assert!(
        plan.query
            .params
            .contains(&SqlValue::Numeric("0.500".into()))
    );
    // Out: a Numeric cell shapes to a JSON string.
    let shaped = shape_rows(
        &["standard_cost".to_string()],
        &[vec![SqlValue::Numeric("12.50".into())]],
    );
    assert_eq!(shaped[0]["standard_cost"], Value::String("12.50".into()));
}

// ---- SECURITY: injection stays a param, identifiers are allowlisted --------

#[test]
fn injection_value_is_a_bound_param_not_interpolated() {
    let cat = catalog();
    // A free-text field: the value is accepted and must stay a parameter.
    // (An enum field would reject this outright — allowlisted values only —
    //  which is why the injection lands in a param here, never in the SQL.)
    let evil = "Acme'; DROP TABLE quality_holds; --";
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/suppliers",
            &q(&[("name", evil)]),
            None,
        )
        .unwrap();
    // The malicious text never appears in the SQL — it is a parameter, verbatim.
    assert!(!plan.query.sql.contains("DROP TABLE"));
    assert!(!plan.query.sql.contains(evil));
    assert!(plan.query.sql.contains("\"name\" = $1"));
    assert_eq!(plan.query.params[0], SqlValue::Text(evil.to_string()));
}

#[test]
fn injection_in_body_value_is_a_bound_param() {
    let cat = catalog();
    let evil = "Acme'); DELETE FROM suppliers; --";
    let body = json!({ "name": evil });
    let plan = Router::new(&cat)
        .compile(Method::Post, "/api/rest/suppliers", &[], Some(&body))
        .unwrap();
    assert!(!plan.query.sql.contains("DELETE FROM"));
    assert!(!plan.query.sql.contains(evil));
    assert_eq!(plan.query.params[0], SqlValue::Text(evil.to_string()));
}

#[test]
fn unknown_entity_is_rejected() {
    let cat = catalog();
    let err = Router::new(&cat)
        .compile(Method::Get, "/api/rest/robert'); DROP", &[], None)
        .unwrap_err();
    assert!(matches!(err, ApiError::UnknownEntity(_)));
    assert_eq!(err.status(), 400);
}

#[test]
fn unknown_filter_column_is_rejected() {
    let cat = catalog();
    let err = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/suppliers",
            &q(&[("evil\"; DROP", "1")]),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, ApiError::UnknownField { .. }));
}

#[test]
fn unknown_sort_column_is_rejected() {
    let cat = catalog();
    let err = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/suppliers",
            &q(&[("sort", "id,injected")]),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, ApiError::UnknownField { .. }));
}

#[test]
fn unknown_expand_relation_is_rejected() {
    let cat = catalog();
    let err = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/suppliers",
            &q(&[("expand", "nope")]),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, ApiError::UnknownRelation { .. }));
}

#[test]
fn body_cannot_set_managed_columns() {
    let cat = catalog();
    for col in ["id", "tenant_id"] {
        let mut m = serde_json::Map::new();
        m.insert("name".into(), json!("X"));
        m.insert(col.to_string(), json!("spoof"));
        let body = Value::Object(m);
        let err = Router::new(&cat)
            .compile(Method::Post, "/api/rest/suppliers", &[], Some(&body))
            .unwrap_err();
        assert!(
            matches!(err, ApiError::InvalidValue { ref field, .. } if field == col),
            "{err:?}"
        );
    }
}

// ---- value validation -----------------------------------------------------

#[test]
fn numeric_float_in_body_is_rejected() {
    let cat = catalog();
    let body = json!({ "name": "M", "moisture_max_pct": 12.34, "weight_tolerance_kg": "0.5" });
    let err = Router::new(&cat)
        .compile(Method::Post, "/api/rest/materials", &[], Some(&body))
        .unwrap_err();
    assert!(
        matches!(err, ApiError::InvalidValue { ref field, .. } if field == "moisture_max_pct"),
        "{err:?}"
    );
}

#[test]
fn numeric_out_of_scale_is_rejected() {
    let cat = catalog();
    // moisture_max_pct is numeric(5,2) → 3 fractional digits is too many.
    let body = json!({ "name": "M", "moisture_max_pct": "1.234", "weight_tolerance_kg": "0.5" });
    let err = Router::new(&cat)
        .compile(Method::Post, "/api/rest/materials", &[], Some(&body))
        .unwrap_err();
    assert!(
        matches!(err, ApiError::InvalidValue { ref field, .. } if field == "moisture_max_pct"),
        "{err:?}"
    );
}

#[test]
fn enum_value_must_be_a_variant() {
    let cat = catalog();
    let err = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/quality_holds",
            &q(&[("status", "eq.bogus")]),
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, ApiError::InvalidValue { ref field, .. } if field == "status"),
        "{err:?}"
    );
}

#[test]
fn non_uuid_id_is_rejected() {
    let cat = catalog();
    let err = Router::new(&cat)
        .compile(Method::Get, "/api/rest/suppliers/not-a-uuid", &[], None)
        .unwrap_err();
    assert!(matches!(err, ApiError::InvalidValue { ref field, .. } if field == "id"));
}

// ---- method / route negatives ---------------------------------------------

#[test]
fn post_with_id_is_method_not_allowed() {
    let cat = catalog();
    let path = format!("/api/rest/suppliers/{A_UUID}");
    let err = Router::new(&cat)
        .compile(Method::Post, &path, &[], Some(&json!({})))
        .unwrap_err();
    assert!(matches!(err, ApiError::MethodNotAllowed));
    assert_eq!(err.status(), 405);
}

#[test]
fn delete_without_id_is_method_not_allowed() {
    let cat = catalog();
    let err = Router::new(&cat)
        .compile(Method::Delete, "/api/rest/suppliers", &[], None)
        .unwrap_err();
    assert!(matches!(err, ApiError::MethodNotAllowed));
}

#[test]
fn base_path_matches_on_a_segment_boundary() {
    let cat = catalog();
    // "/api/restaurants" must NOT match base "/api/rest".
    let err = Router::new(&cat)
        .compile(Method::Get, "/api/restaurants", &[], None)
        .unwrap_err();
    assert!(matches!(err, ApiError::NotFound));
}

#[test]
fn method_from_http_is_case_insensitive() {
    assert_eq!(Method::from_http("get"), Some(Method::Get));
    assert_eq!(Method::from_http("PATCH"), Some(Method::Patch));
    assert_eq!(Method::from_http("TRACE"), None);
}
