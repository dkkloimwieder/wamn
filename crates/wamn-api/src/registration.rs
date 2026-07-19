//! Minimal CRUD for **event registrations** (EVT-REG, D19 v3 §5) over the fixed
//! `catalog.event_registrations` store (deploy/sql/catalog-schema.sql).
//!
//! A registration is a subscribing flow's declaration — entity id, ops,
//! condition, partition-key expr — modelled and validated by
//! [`wamn_event_reg::EventRegistration`]; the materializer (l5i9.17) consumes
//! it. This module is the management surface: enough for a client to
//! create/list/get/update/delete registrations, no extras (the editor panel is
//! later, EVT-TRIGGER-UX; parked per the core-pivot rule).
//!
//! Style matches the rest of the gateway and `wamn_migrate::sql`: **identifiers
//! are pinned** (the fixed metadata schema — not catalog-driven, so no
//! allowlist step), and **every value is an `$n` parameter**. `tenant_id` is
//! never taken from the caller: an INSERT sets it from the `app.tenant` session
//! claim server-side (the 3.2 floor's `WITH CHECK`), and every read/update/delete
//! is scoped by the RLS `catalog.event_registrations` policy — so no tenant value
//! is bound here, and `tenant_id` is never projected. The `document` is bound as
//! [`SqlValue::Json`] and cast `$n::jsonb`, so a caller passes the serialized
//! [`wamn_event_reg::EventRegistration`] and the denormalized keys
//! (`catalog_id`/`registration_id`/`flow_id`/`entity_id`, which must equal the
//! document's own values — validated by `wamn_event_reg::validate` before write).

use crate::router::Compiled;
use crate::value::SqlValue;

/// The columns a read (or a write's `RETURNING`) projects, in order. `tenant_id`
/// is deliberately never exposed; the full declaration comes back as
/// `registration` (a `jsonb` column → a real JSON value when shaped).
const COLUMNS: &[&str] = &["registration_id", "flow_id", "entity_id", "registration"];

fn read_columns() -> Vec<String> {
    COLUMNS.iter().map(|s| (*s).to_string()).collect()
}

/// Insert one registration. `tenant_id` is the session claim (server-side);
/// `document` is the full [`wamn_event_reg::EventRegistration`] JSON stored as
/// `jsonb`. Fails at the DB on a duplicate `(tenant, catalog, registration_id)`
/// (the primary key).
pub fn create(
    catalog_id: &str,
    registration_id: &str,
    flow_id: &str,
    entity_id: &str,
    document: &str,
) -> Compiled {
    Compiled {
        sql: "INSERT INTO catalog.event_registrations \
                (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
              VALUES (current_setting('app.tenant', true), $1, $2, $3, $4, $5::jsonb) \
              RETURNING registration_id, flow_id, entity_id, registration"
            .to_string(),
        params: vec![
            SqlValue::Text(catalog_id.to_string()),
            SqlValue::Text(registration_id.to_string()),
            SqlValue::Text(flow_id.to_string()),
            SqlValue::Text(entity_id.to_string()),
            SqlValue::Json(document.to_string()),
        ],
        columns: read_columns(),
    }
}

/// List every registration in a catalog (tenant-scoped by RLS), ordered by id.
pub fn list(catalog_id: &str) -> Compiled {
    Compiled {
        sql: "SELECT registration_id, flow_id, entity_id, registration \
              FROM catalog.event_registrations \
              WHERE catalog_id = $1 \
              ORDER BY registration_id ASC"
            .to_string(),
        params: vec![SqlValue::Text(catalog_id.to_string())],
        columns: read_columns(),
    }
}

/// Read one registration by id (tenant-scoped by RLS).
pub fn get(catalog_id: &str, registration_id: &str) -> Compiled {
    Compiled {
        sql: "SELECT registration_id, flow_id, entity_id, registration \
              FROM catalog.event_registrations \
              WHERE catalog_id = $1 AND registration_id = $2"
            .to_string(),
        params: vec![
            SqlValue::Text(catalog_id.to_string()),
            SqlValue::Text(registration_id.to_string()),
        ],
        columns: read_columns(),
    }
}

/// Replace a registration's mutable fields (`flow_id`, `entity_id`, and the
/// `document`) by id; the `(catalog_id, registration_id)` key is immutable.
/// `RETURNING` yields no row when the id is absent (a 404 for the caller).
pub fn update(
    catalog_id: &str,
    registration_id: &str,
    flow_id: &str,
    entity_id: &str,
    document: &str,
) -> Compiled {
    Compiled {
        sql: "UPDATE catalog.event_registrations \
              SET flow_id = $1, entity_id = $2, registration = $3::jsonb \
              WHERE catalog_id = $4 AND registration_id = $5 \
              RETURNING registration_id, flow_id, entity_id, registration"
            .to_string(),
        params: vec![
            SqlValue::Text(flow_id.to_string()),
            SqlValue::Text(entity_id.to_string()),
            SqlValue::Json(document.to_string()),
            SqlValue::Text(catalog_id.to_string()),
            SqlValue::Text(registration_id.to_string()),
        ],
        columns: read_columns(),
    }
}

/// Delete a registration by id. `RETURNING registration_id` yields no row when
/// the id is absent (a 404 for the caller).
pub fn delete(catalog_id: &str, registration_id: &str) -> Compiled {
    Compiled {
        sql: "DELETE FROM catalog.event_registrations \
              WHERE catalog_id = $1 AND registration_id = $2 \
              RETURNING registration_id"
            .to_string(),
        params: vec![
            SqlValue::Text(catalog_id.to_string()),
            SqlValue::Text(registration_id.to_string()),
        ],
        columns: vec!["registration_id".to_string()],
    }
}
