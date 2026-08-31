//! Explicit application-table substrate shared by the WAL and CDC campaigns.
//!
//! These tables are measurement fixtures, not generated application contracts.
//! Keeping their DDL beside the campaigns avoids retaining the superseded
//! catalog-model/compiler solely to mint benchmark setup.

use std::path::Path;

use wamn_schema_control::ManagedModel;

pub const PACKAGE_ID: &str = "poc_material_receiving";

const MODEL_TABLES: [(&str, &str); 9] = [
    ("users", "users"),
    ("sites", "sites"),
    ("suppliers", "suppliers"),
    ("materials", "materials"),
    ("receipts", "receipts"),
    ("receipt_lines", "receipt_lines"),
    ("quality_holds", "quality_holds"),
    ("dispositions", "dispositions"),
    ("disposition_reviews", "disposition_reviews"),
];

pub fn models(schema: &str) -> Vec<ManagedModel> {
    MODEL_TABLES
        .into_iter()
        .map(|(model_id, table)| ManagedModel {
            model_id: model_id.to_owned(),
            schema: schema.to_owned(),
            table: table.to_owned(),
        })
        .collect()
}

pub fn model_tables() -> impl Iterator<Item = (&'static str, &'static str)> {
    MODEL_TABLES.into_iter()
}

/// Render the package-owned measurement DDL with explicit schema selection.
pub fn floor_sql(schema: &str) -> String {
    assert!(
        !schema.is_empty()
            && schema
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
        "measurement schema must be a lowercase identifier"
    );
    FLOOR_SQL_TEMPLATE.replace("__SCHEMA__", schema)
}

/// Write the strict package directory consumed by the real RI CLI.
pub fn write_package_directory(root: &Path, schema: &str) -> anyhow::Result<()> {
    let models = model_tables()
        .map(|(model_id, table)| {
            (
                model_id.to_owned(),
                serde_json::json!({
                    "schema": schema,
                    "table": table,
                    "owner": PACKAGE_ID,
                    "operations": {}
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let manifest = serde_json::json!({
        "package": {"id": PACKAGE_ID, "version": "1.0.0"},
        "required_platform_policy_contract": {
            "id": "measurement_data_access",
            "state": "unsatisfied"
        },
        "models": models,
        "connections": {},
        "components": {}
    });
    std::fs::create_dir_all(root.join("migrations"))?;
    std::fs::write(
        root.join("wamn.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    std::fs::write(root.join("migrations/0001_initial.sql"), floor_sql(schema))?;
    Ok(())
}

const FLOOR_SQL_TEMPLATE: &str = r#"
CREATE TABLE __SCHEMA__.users (
    id uuid CONSTRAINT users_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id text NOT NULL,
    email varchar(320) NOT NULL,
    display_name text,
    cert_level text CONSTRAINT users_cert_level_check CHECK (cert_level IN ('L1', 'L2'))
);
CREATE TABLE __SCHEMA__.sites (
    id uuid CONSTRAINT sites_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id text NOT NULL,
    name text NOT NULL,
    code varchar(16) NOT NULL
);
CREATE TABLE __SCHEMA__.suppliers (
    id uuid CONSTRAINT suppliers_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id text NOT NULL,
    name text NOT NULL,
    contact_email text,
    standard_cost numeric(12,2)
);
CREATE TABLE __SCHEMA__.materials (
    id uuid CONSTRAINT materials_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id text NOT NULL,
    name text NOT NULL,
    moisture_max_pct numeric(5,2) NOT NULL,
    weight_tolerance_kg numeric(8,3) NOT NULL
);
CREATE TABLE __SCHEMA__.receipts (
    id uuid CONSTRAINT receipts_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id text NOT NULL,
    receipt_no varchar(64) NOT NULL,
    supplier_id uuid NOT NULL CONSTRAINT receipts_supplier_id_fkey REFERENCES __SCHEMA__.suppliers (id),
    site_id uuid NOT NULL CONSTRAINT receipts_site_id_fkey REFERENCES __SCHEMA__.sites (id),
    received_at timestamptz NOT NULL
);
CREATE TABLE __SCHEMA__.receipt_lines (
    id uuid CONSTRAINT receipt_lines_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id text NOT NULL,
    receipt_id uuid NOT NULL CONSTRAINT receipt_lines_receipt_id_fkey REFERENCES __SCHEMA__.receipts (id),
    line_no int NOT NULL,
    material_id uuid NOT NULL CONSTRAINT receipt_lines_material_id_fkey REFERENCES __SCHEMA__.materials (id),
    quantity numeric(12,3) NOT NULL
);
CREATE TABLE __SCHEMA__.quality_holds (
    id uuid CONSTRAINT quality_holds_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id text NOT NULL,
    line_id uuid NOT NULL CONSTRAINT quality_holds_line_id_fkey REFERENCES __SCHEMA__.receipt_lines (id),
    site_id uuid NOT NULL CONSTRAINT quality_holds_site_id_fkey REFERENCES __SCHEMA__.sites (id),
    status text NOT NULL CONSTRAINT quality_holds_status_check
        CHECK (status IN ('open', 'disposed', 'escalated')),
    opened_at timestamptz NOT NULL
);
CREATE TABLE __SCHEMA__.dispositions (
    id uuid CONSTRAINT dispositions_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id text NOT NULL,
    hold_id uuid NOT NULL CONSTRAINT dispositions_hold_id_fkey REFERENCES __SCHEMA__.quality_holds (id),
    inspector_id uuid NOT NULL CONSTRAINT dispositions_inspector_id_fkey REFERENCES __SCHEMA__.users (id),
    decision text NOT NULL CONSTRAINT dispositions_decision_check
        CHECK (decision IN ('accept', 'reject', 'use-as-is')),
    decided_at timestamptz NOT NULL
);
CREATE TABLE __SCHEMA__.disposition_reviews (
    id uuid CONSTRAINT disposition_reviews_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id text NOT NULL,
    disposition_id uuid NOT NULL CONSTRAINT disposition_reviews_disposition_id_fkey
        REFERENCES __SCHEMA__.dispositions (id),
    recommendation text NOT NULL CONSTRAINT disposition_reviews_recommendation_check
        CHECK (recommendation IN ('accept', 'reject', 'use-as-is')),
    confidence varchar(32) NOT NULL,
    matched boolean NOT NULL
);

DO $tenant_floor$
DECLARE
    relation_name text;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'users', 'sites', 'suppliers', 'materials', 'receipts', 'receipt_lines',
        'quality_holds', 'dispositions', 'disposition_reviews'
    ] LOOP
        EXECUTE format('ALTER TABLE %I.%I ENABLE ROW LEVEL SECURITY', '__SCHEMA__', relation_name);
        EXECUTE format('ALTER TABLE %I.%I FORCE ROW LEVEL SECURITY', '__SCHEMA__', relation_name);
        EXECUTE format(
            'CREATE POLICY %I ON %I.%I TO wamn_app USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()) WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())',
            relation_name || '_tenant', '__SCHEMA__', relation_name
        );
        EXECUTE format(
            'GRANT SELECT, INSERT, UPDATE, DELETE ON %I.%I TO wamn_app',
            '__SCHEMA__', relation_name
        );
    END LOOP;
END
$tenant_floor$;
"#;
