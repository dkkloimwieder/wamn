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
use wamn_catalog::{Cardinality, Catalog, Entity, Field, FieldType, Relation};

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
    assert_eq!(plan.kind(), PlanKind::List);
    // id + the three user fields, in order; tenant_id absent.
    assert!(plan.query().sql().starts_with(
        "SELECT \"id\", \"name\", \"contact_email\", \"standard_cost\" FROM \"suppliers\""
    ));
    assert!(!plan.query().sql().contains("tenant_id"));
    // default order + a capped page.
    assert!(plan.query().sql().contains("ORDER BY \"id\" ASC"));
    assert!(plan.query().sql().contains("LIMIT $1 OFFSET $2"));
    assert_eq!(
        plan.query().params(),
        vec![SqlValue::Int64(50), SqlValue::Int64(0)]
    );
    assert_eq!(
        plan.query().columns(),
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
    assert_eq!(plan.kind(), PlanKind::GetOne);
    assert_eq!(
        plan.query().sql(),
        "SELECT \"id\", \"name\", \"contact_email\", \"standard_cost\" FROM \"suppliers\" WHERE \"id\" = $1"
    );
    assert_eq!(
        plan.query().params(),
        vec![SqlValue::Uuid(A_UUID.to_string())]
    );
}

#[test]
fn create_sets_tenant_from_claim_and_returns_projection() {
    let cat = catalog();
    let body = json!({ "name": "Acme", "standard_cost": "12.50" });
    let plan = Router::new(&cat)
        .compile(Method::Post, "/api/rest/suppliers", &[], Some(&body))
        .unwrap();
    assert_eq!(plan.kind(), PlanKind::CreateOne);
    assert_eq!(plan.status(), 201);
    // tenant_id set server-side from the claim (not a param), user cols bound.
    assert_eq!(
        plan.query().sql(),
        "INSERT INTO \"suppliers\" (\"tenant_id\", \"name\", \"standard_cost\") \
         VALUES (current_setting('app.tenant', true), $1, $2) \
         RETURNING \"id\", \"name\", \"contact_email\", \"standard_cost\""
    );
    assert_eq!(
        plan.query().params(),
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
fn rejects_infinite_timestamps_at_both_value_paths() {
    // `+/-infinity` are valid instants Postgres would serialize via `to_jsonb`
    // as the JSON string "infinity" in a JSON row-event payload, silently
    // changing the field's JSON type. The gateway rejects them at the edge
    // (wamn-oj7), on both the JSON-body and query-filter value paths.
    let cat = catalog();
    let path = format!("/api/rest/receipts/{A_UUID}"); // receipts.received_at is timestamptz

    // JSON body → value_for_field (PATCH types only the provided field).
    for spelling in ["infinity", "-infinity", "Infinity", "inf", " infinity "] {
        let err = Router::new(&cat)
            .compile(
                Method::Patch,
                &path,
                &[],
                Some(&json!({ "received_at": spelling })),
            )
            .unwrap_err();
        assert!(
            matches!(err, ApiError::InvalidValue { ref field, .. } if field == "received_at"),
            "PATCH received_at={spelling:?}: {err:?}"
        );
        assert_eq!(err.status(), 400);
    }
    // A finite instant is accepted.
    let ok = Router::new(&cat)
        .compile(
            Method::Patch,
            &path,
            &[],
            Some(&json!({ "received_at": "2026-07-13T00:00:00Z" })),
        )
        .unwrap();
    assert_eq!(ok.kind(), PlanKind::UpdateOne);

    // Query filter → value_for_field_str.
    for spelling in ["infinity", "-infinity", "inf"] {
        let err = Router::new(&cat)
            .compile(
                Method::Get,
                "/api/rest/receipts",
                &q(&[("received_at", spelling)]),
                None,
            )
            .unwrap_err();
        assert!(
            matches!(err, ApiError::InvalidValue { ref field, .. } if field == "received_at"),
            "GET ?received_at={spelling}: {err:?}"
        );
    }
    // A finite instant filters fine.
    Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/receipts",
            &q(&[("received_at", "2026-07-13T00:00:00Z")]),
            None,
        )
        .expect("a finite timestamp filter compiles");
}

#[test]
fn numeric_nan_is_still_rejected() {
    // Regression: the gateway already rejects `NaN` on a numeric field (no
    // oj7 change there — `validate_decimal` treats non-digit bytes as "not a
    // number"). suppliers.standard_cost is numeric(12,2).
    let cat = catalog();
    let path = format!("/api/rest/suppliers/{A_UUID}");
    let err = Router::new(&cat)
        .compile(
            Method::Patch,
            &path,
            &[],
            Some(&json!({ "standard_cost": "NaN" })),
        )
        .unwrap_err();
    assert!(
        matches!(err, ApiError::InvalidValue { ref field, .. } if field == "standard_cost"),
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
    assert_eq!(plan.kind(), PlanKind::UpdateOne);
    assert_eq!(
        plan.query().sql(),
        "UPDATE \"suppliers\" SET \"contact_email\" = $1 WHERE \"id\" = $2 \
         RETURNING \"id\", \"name\", \"contact_email\", \"standard_cost\""
    );
    assert_eq!(
        plan.query().params(),
        vec![
            SqlValue::Text("new@acme.test".into()),
            SqlValue::Uuid(A_UUID.to_string())
        ]
    );
}

#[test]
fn patch_empty_body_is_rejected() {
    let cat = catalog();
    let path = format!("/api/rest/suppliers/{A_UUID}");
    // PATCH is a partial merge — a body with nothing to set is a 400.
    let err = Router::new(&cat)
        .compile(Method::Patch, &path, &[], Some(&json!({})))
        .unwrap_err();
    assert!(matches!(err, ApiError::InvalidRequest(_)), "{err:?}");
    assert_eq!(err.status(), 400);
}

#[test]
fn put_full_replace_resets_omitted_optional_fields_to_default() {
    let cat = catalog();
    let path = format!("/api/rest/suppliers/{A_UUID}");
    // Only the required `name` is present; the two nullable, no-default fields
    // are omitted → a full replace resets them to their column DEFAULT (NULL
    // here), NOT left untouched as PATCH would.
    let body = json!({ "name": "Renamed" });
    let plan = Router::new(&cat)
        .compile(Method::Put, &path, &[], Some(&body))
        .unwrap();
    assert_eq!(plan.kind(), PlanKind::UpdateOne);
    assert_eq!(plan.status(), 200);
    assert_eq!(
        plan.query().sql(),
        "UPDATE \"suppliers\" SET \"name\" = $1, \"contact_email\" = DEFAULT, \
         \"standard_cost\" = DEFAULT WHERE \"id\" = $2 \
         RETURNING \"id\", \"name\", \"contact_email\", \"standard_cost\""
    );
    // DEFAULT is a keyword, not a param — only the present value + id are bound.
    assert_eq!(
        plan.query().params(),
        vec![
            SqlValue::Text("Renamed".into()),
            SqlValue::Uuid(A_UUID.to_string())
        ]
    );
}

#[test]
fn put_rejects_missing_required_field() {
    let cat = catalog();
    let path = format!("/api/rest/suppliers/{A_UUID}");
    // suppliers.name is NOT NULL with no default → a full replace that omits it
    // is a 400, reported identically to the create path (InvalidValue{name}).
    let body = json!({ "contact_email": "x@y.z" });
    let err = Router::new(&cat)
        .compile(Method::Put, &path, &[], Some(&body))
        .unwrap_err();
    assert!(
        matches!(err, ApiError::InvalidValue { ref field, .. } if field == "name"),
        "{err:?}"
    );
    assert_eq!(err.status(), 400);
}

#[test]
fn put_with_all_fields_binds_each_as_a_param() {
    let cat = catalog();
    let path = format!("/api/rest/suppliers/{A_UUID}");
    // Every writable field present → a normal UPDATE, each value a $n param and
    // no DEFAULT keyword; id bound last.
    let body = json!({ "name": "Acme", "contact_email": "a@acme.test", "standard_cost": "9.99" });
    let plan = Router::new(&cat)
        .compile(Method::Put, &path, &[], Some(&body))
        .unwrap();
    assert_eq!(plan.kind(), PlanKind::UpdateOne);
    assert!(
        !plan.query().sql().contains("DEFAULT"),
        "{}",
        plan.query().sql()
    );
    assert_eq!(
        plan.query().sql(),
        "UPDATE \"suppliers\" SET \"name\" = $1, \"contact_email\" = $2, \
         \"standard_cost\" = $3 WHERE \"id\" = $4 \
         RETURNING \"id\", \"name\", \"contact_email\", \"standard_cost\""
    );
    assert_eq!(
        plan.query().params(),
        vec![
            SqlValue::Text("Acme".into()),
            SqlValue::Text("a@acme.test".into()),
            SqlValue::Numeric("9.99".into()),
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
    assert_eq!(plan.kind(), PlanKind::DeleteOne);
    assert_eq!(plan.status(), 204);
    assert_eq!(
        plan.query().sql(),
        "DELETE FROM \"suppliers\" WHERE \"id\" = $1 RETURNING \"id\""
    );
    assert_eq!(
        plan.query().params(),
        vec![SqlValue::Uuid(A_UUID.to_string())]
    );
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
        plan.query().sql().contains("WHERE \"status\" = $1"),
        "{}",
        plan.query().sql()
    );
    assert_eq!(plan.query().params()[0], SqlValue::Text("open".into()));
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
    assert!(plan.query().sql().contains("\"standard_cost\" = $1"));
    assert_eq!(plan.query().params()[0], SqlValue::Numeric("12.50".into()));
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
        plan.query().sql().contains("\"status\" IN ($1, $2)"),
        "{}",
        plan.query().sql()
    );
    assert_eq!(plan.query().params()[0], SqlValue::Text("open".into()));
    assert_eq!(plan.query().params()[1], SqlValue::Text("escalated".into()));
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
    // The unique `id` tiebreaker is always appended as the final ORDER BY key so
    // OFFSET pagination stays stable under a sort on non-unique columns (C5-1).
    assert!(
        plan.query()
            .sql()
            .contains("ORDER BY \"received_at\" ASC, \"receipt_no\" DESC, \"id\" ASC")
    );
    // limit clamped to the max page size (100), offset passed through, both params.
    let n = plan.query().params().len();
    assert_eq!(plan.query().params()[n - 2], SqlValue::Int64(100));
    assert_eq!(plan.query().params()[n - 1], SqlValue::Int64(40));
}

#[test]
fn user_sort_still_appends_the_id_tiebreaker() {
    // A sort on a single non-unique column must still end with the unique `id`
    // tiebreaker so OFFSET pages neither skip nor duplicate rows (C5-1).
    let cat = catalog();
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/quality_holds",
            &q(&[("sort", "status")]),
            None,
        )
        .unwrap();
    assert!(
        plan.query()
            .sql()
            .contains("ORDER BY \"status\" ASC, \"id\" ASC"),
        "sql was: {}",
        plan.query().sql()
    );
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
    assert_eq!(plan.expands().len(), 1);
    let ex = &plan.expands()[0];
    assert_eq!(ex.dir(), ExpandDir::ToOne);
    assert_eq!(ex.key_column(), "line_id");
    assert_eq!(ex.target_table(), "receipt_lines");
    assert_eq!(ex.match_column(), "id");

    let keys = vec![SqlValue::Uuid(A_UUID.into()), SqlValue::Uuid(B_UUID.into())];
    let sub = Router::new(&cat).build_expand(ex, &keys);
    assert_eq!(
        sub.sql(),
        "SELECT \"id\", \"receipt_id\", \"material_id\", \"quantity\" FROM \"receipt_lines\" WHERE \"id\" IN ($1, $2)"
    );
    assert_eq!(sub.params(), keys);
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
    let ex = &plan.expands()[0];
    assert_eq!(ex.dir(), ExpandDir::ToMany);
    assert_eq!(ex.key_column(), "id");
    assert_eq!(ex.target_table(), "receipt_lines");
    assert_eq!(ex.match_column(), "receipt_id");
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
    let ex = &plan.expands()[0];

    // Two holds, both pointing at line A.
    let mut primary = shape_rows(
        plan.query().columns(),
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
    attach_expansion(&mut primary, ex, ex.columns(), &expanded_rows);
    let line = &primary[0]["line"];
    assert_eq!(line["id"], json!(A_UUID));
    assert_eq!(line["quantity"], json!("3.000")); // exact-decimal string, not a float
}

#[test]
fn duplicate_expand_names_are_deduped_first_occurrence_order() {
    // A repeated relation name — within one `expand=` value or across several —
    // must collapse to a single Expand (one DB round-trip), first-occurrence
    // order preserved. Without the dedup, `?expand=lines,lines,…` amplifies into
    // one identical expansion query per token (cjv.13). receipt_lines expands two
    // distinct relations: `lines` (parent receipt) and `material`.
    let cat = catalog();

    // List path: duplicates within a single value collapse to [lines, material].
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/receipt_lines",
            &q(&[("expand", "lines,lines,material,lines")]),
            None,
        )
        .unwrap();
    let names: Vec<&str> = plan.expands().iter().map(|e| e.name()).collect();
    assert_eq!(names, vec!["lines", "material"], "list path");

    // Get path: same collapse on the single-resource route.
    let path = format!("/api/rest/receipt_lines/{A_UUID}");
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            &path,
            &q(&[("expand", "lines,lines,material,lines")]),
            None,
        )
        .unwrap();
    let names: Vec<&str> = plan.expands().iter().map(|e| e.name()).collect();
    assert_eq!(names, vec!["lines", "material"], "get path");

    // Repeated `expand=` params also collapse (collect_names appends across them).
    let plan = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/receipt_lines",
            &q(&[
                ("expand", "lines"),
                ("expand", "material"),
                ("expand", "lines"),
            ]),
            None,
        )
        .unwrap();
    let names: Vec<&str> = plan.expands().iter().map(|e| e.name()).collect();
    assert_eq!(names, vec!["lines", "material"], "repeated params");
}

// ---- expansion cardinality (cjv.14) ---------------------------------------
//
// The POC fixture is all one-to-many, so these use an in-code catalog carrying
// the shapes it lacks: a many-to-many, a self-referential hierarchical, and a
// one-to-many missing its backing FK field. Built directly — `Catalog::from_json`
// (how the fixture loads) does no validation, so these need not pass `validate()`
// — and touching no shared fixture.

fn odd_catalog() -> Catalog {
    Catalog {
        schema_version: "0.1".to_string(),
        catalog_id: "cjv14".to_string(),
        version: 1,
        name: None,
        entities: vec![
            ent("articles", vec![text("title"), reference("tag_id", "tags")]),
            ent("tags", vec![text("label")]),
            ent(
                "article_tags",
                vec![
                    reference("article_id", "articles"),
                    reference("tag_id", "tags"),
                ],
            ),
            ent(
                "categories",
                vec![text("cat_name"), reference("parent_id", "categories")],
            ),
            ent("authors", vec![text("author_name")]),
        ],
        relations: vec![
            // A stray from_field is deliberately present so the "silently
            // mis-served as a to-one" mutant has a field to serve against; the fix
            // must reject by cardinality regardless.
            rel(
                "m2m",
                "tags",
                Cardinality::ManyToMany,
                "articles",
                "tags",
                Some("tag_id"),
                Some("article_tags"),
            ),
            rel(
                "tree",
                "subcategories",
                Cardinality::Hierarchical,
                "categories",
                "categories",
                Some("parent_id"),
                None,
            ),
            rel(
                "auth",
                "author",
                Cardinality::OneToMany,
                "articles",
                "authors",
                None,
                None,
            ),
        ],
    }
}

fn ent(name: &str, fields: Vec<Field>) -> Entity {
    Entity {
        id: name.into(),
        name: name.to_string(),
        is_system: false,
        label: None,
        description: None,
        fields,
        indexes: Vec::new(),
        constraints: Vec::new(),
    }
}

fn text(name: &str) -> Field {
    field(name, FieldType::Text { max_len: None })
}

fn reference(name: &str, target: &str) -> Field {
    field(
        name,
        FieldType::Reference {
            entity: target.into(),
        },
    )
}

fn field(name: &str, field_type: FieldType) -> Field {
    Field {
        id: name.into(),
        name: name.to_string(),
        field_type,
        nullable: true,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    }
}

fn rel(
    id: &str,
    name: &str,
    cardinality: Cardinality,
    from: &str,
    to: &str,
    from_field: Option<&str>,
    through: Option<&str>,
) -> Relation {
    Relation {
        id: id.to_string(),
        name: name.to_string(),
        cardinality,
        from: from.into(),
        to: to.into(),
        from_field: from_field.map(Into::into),
        through: through.map(Into::into),
        description: None,
    }
}

#[test]
fn many_to_many_expansion_is_unsupported_not_unknown() {
    // A many-to-many relation is rejected by cardinality with a distinct error —
    // never mis-served as a direct FK read against the wrong table (cjv.14).
    let cat = odd_catalog();
    let err = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/articles",
            &q(&[("expand", "tags")]),
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, ApiError::UnsupportedExpansion { ref relation, cardinality, .. }
            if relation == "tags" && cardinality == "many-to-many"),
        "{err:?}"
    );
    assert_eq!(err.status(), 400);
}

#[test]
fn hierarchical_expansion_is_unsupported_not_to_one() {
    // A self-referential hierarchical relation used to always take the to-one
    // branch; now it is cleanly rejected as unsupported (cjv.14).
    let cat = odd_catalog();
    let err = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/categories",
            &q(&[("expand", "subcategories")]),
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, ApiError::UnsupportedExpansion { ref relation, cardinality, .. }
            if relation == "subcategories" && cardinality == "hierarchical"),
        "{err:?}"
    );
    assert_eq!(err.status(), 400);
}

#[test]
fn one_to_many_missing_from_field_is_unservable_not_unknown() {
    // A matched one-to-many with no backing FK field is a malformed/unservable
    // relation — reported distinctly from a truly-unknown relation (cjv.14).
    let cat = odd_catalog();
    let err = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/articles",
            &q(&[("expand", "author")]),
            None,
        )
        .unwrap_err();
    assert!(!matches!(err, ApiError::UnknownRelation { .. }), "{err:?}");
    assert!(
        matches!(err, ApiError::UnservableRelation { ref relation, .. } if relation == "author"),
        "{err:?}"
    );
    assert_eq!(err.status(), 400);
}

#[test]
fn truly_unknown_relation_is_still_unknown() {
    // The unknown-name path is unchanged by the cardinality switch: a name that
    // matches no relation is still UnknownRelation, not the new variants.
    let cat = odd_catalog();
    let err = Router::new(&cat)
        .compile(
            Method::Get,
            "/api/rest/articles",
            &q(&[("expand", "nope")]),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, ApiError::UnknownRelation { .. }), "{err:?}");
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
        plan.query()
            .params()
            .contains(&SqlValue::Numeric("12.34".into()))
    );
    assert!(
        plan.query()
            .params()
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
    assert!(!plan.query().sql().contains("DROP TABLE"));
    assert!(!plan.query().sql().contains(evil));
    assert!(plan.query().sql().contains("\"name\" = $1"));
    assert_eq!(plan.query().params()[0], SqlValue::Text(evil.to_string()));
}

#[test]
fn injection_in_body_value_is_a_bound_param() {
    let cat = catalog();
    let evil = "Acme'); DELETE FROM suppliers; --";
    let body = json!({ "name": evil });
    let plan = Router::new(&cat)
        .compile(Method::Post, "/api/rest/suppliers", &[], Some(&body))
        .unwrap();
    assert!(!plan.query().sql().contains("DELETE FROM"));
    assert!(!plan.query().sql().contains(evil));
    assert_eq!(plan.query().params()[0], SqlValue::Text(evil.to_string()));
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
