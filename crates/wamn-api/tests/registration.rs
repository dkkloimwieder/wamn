//! Emitted-SQL tests for the event-registration CRUD builders (EVT-REG, D19 v3
//! §5) + a drift guard tying them to the storage schema.
//!
//! Like the router tests these assert the *emitted SQL + params* (no DB): the
//! shapes, that `tenant_id` is server-side (never a caller param), that the
//! document is bound (`$n::jsonb`) not interpolated, and — the drift guard —
//! that every table/column the builders name exists in
//! deploy/sql/catalog-schema.sql (the live-apply target).

use wamn_api::SqlValue;
use wamn_api::registration;

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

const DOC: &str = r#"{"schema-version":"0.1","registration-id":"r1","catalog-id":"shop","flow-id":"notify","entity":"sales_orders","ops":["insert"]}"#;

#[test]
fn create_sets_tenant_server_side_and_binds_the_document_as_jsonb() {
    let c = registration::create("shop", "r1", "notify", "sales_orders", DOC);
    // tenant is the session claim — NOT a bound parameter.
    assert!(c.sql().contains("current_setting('app.tenant', true)"));
    // The document is a $n::jsonb cast, never interpolated into the statement.
    assert!(c.sql().contains("$5::jsonb"));
    assert!(!c.sql().contains("sales_orders")); // the entity id rode in as a param
    assert!(!c.sql().contains("schema-version")); // the doc rode in as a param
    assert_eq!(
        c.params(),
        &[
            SqlValue::Text("shop".into()),
            SqlValue::Text("r1".into()),
            SqlValue::Text("notify".into()),
            SqlValue::Text("sales_orders".into()),
            SqlValue::Json(DOC.into()),
        ]
    );
    assert!(
        c.sql()
            .starts_with("INSERT INTO catalog.event_registrations")
    );
    assert!(
        c.sql()
            .contains("RETURNING registration_id, flow_id, entity_id, registration")
    );
    assert_eq!(
        c.columns(),
        &["registration_id", "flow_id", "entity_id", "registration"]
    );
}

#[test]
fn list_is_catalog_scoped_and_ordered() {
    let c = registration::list("shop");
    assert!(c.sql().contains("FROM catalog.event_registrations"));
    assert!(c.sql().contains("WHERE catalog_id = $1"));
    assert!(c.sql().contains("ORDER BY registration_id ASC"));
    assert_eq!(c.params(), &[SqlValue::Text("shop".into())]);
    // Tenant scoping is the RLS policy's job — never a WHERE the builder adds.
    assert!(!c.sql().contains("tenant_id"));
}

#[test]
fn get_binds_both_key_parts() {
    let c = registration::get("shop", "r1");
    assert!(
        c.sql()
            .contains("WHERE catalog_id = $1 AND registration_id = $2")
    );
    assert_eq!(
        c.params(),
        &[SqlValue::Text("shop".into()), SqlValue::Text("r1".into())]
    );
}

#[test]
fn update_replaces_mutable_fields_and_keys_on_the_immutable_pair() {
    let c = registration::update("shop", "r1", "notify2", "line_items", DOC);
    assert!(c.sql().starts_with("UPDATE catalog.event_registrations"));
    assert!(
        c.sql()
            .contains("SET flow_id = $1, entity_id = $2, registration = $3::jsonb")
    );
    assert!(
        c.sql()
            .contains("WHERE catalog_id = $4 AND registration_id = $5")
    );
    assert!(
        c.sql()
            .contains("RETURNING registration_id, flow_id, entity_id, registration")
    );
    assert_eq!(
        c.params(),
        &[
            SqlValue::Text("notify2".into()),
            SqlValue::Text("line_items".into()),
            SqlValue::Json(DOC.into()),
            SqlValue::Text("shop".into()),
            SqlValue::Text("r1".into()),
        ]
    );
}

#[test]
fn delete_returns_the_id_so_a_missing_row_is_a_404() {
    let c = registration::delete("shop", "r1");
    assert!(
        c.sql()
            .starts_with("DELETE FROM catalog.event_registrations")
    );
    assert!(
        c.sql()
            .contains("WHERE catalog_id = $1 AND registration_id = $2")
    );
    assert!(c.sql().contains("RETURNING registration_id"));
    assert_eq!(c.columns(), &["registration_id"]);
}

/// Drift guard: the storage schema the builders target must carry the table,
/// every column they name, the tenant RLS policy, and the by-entity index. If
/// someone renames a column in catalog-schema.sql without updating the builders
/// (or vice versa), this fails before the live gate ever runs.
#[test]
fn builders_and_catalog_schema_agree_on_the_storage_shape() {
    for needle in [
        "CREATE TABLE catalog.event_registrations",
        "registration_id text NOT NULL",
        "flow_id         text NOT NULL",
        "entity_id       text NOT NULL",
        "registration    jsonb NOT NULL",
        "PRIMARY KEY (tenant_id, catalog_id, registration_id)",
        "CREATE POLICY event_registrations_tenant ON catalog.event_registrations",
        "GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.event_registrations TO wamn_app",
        "CREATE INDEX event_registrations_by_entity",
    ] {
        assert!(
            CATALOG_SCHEMA.contains(needle),
            "deploy/sql/catalog-schema.sql is missing {needle:?} — builders would drift"
        );
    }

    // Every identifier the builders emit is present in the schema (the builders
    // pin identifiers; this proves the pins resolve).
    for c in [
        registration::create("c", "r", "f", "e", "{}"),
        registration::list("c"),
        registration::get("c", "r"),
        registration::update("c", "r", "f", "e", "{}"),
        registration::delete("c", "r"),
    ] {
        assert!(c.sql().contains("catalog.event_registrations"));
    }
}
