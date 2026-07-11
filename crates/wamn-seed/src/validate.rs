//! Structural validation of a [`Dataset`] against the catalog it populates.
//!
//! Checks that rows resolve and type-check — entities/fields exist, values match
//! field types (enums are variants, numerics are exact decimals with no float,
//! uuids parse), references point at seeded keys, required fields are present,
//! and per-entity keys and unique tuples are distinct — reusing the catalog's
//! [`Issue`] / [`Severity`] shape (3.1). It does not apply anything.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use wamn_catalog::{Catalog, Constraint, Entity, FieldType, Issue, Severity};

use crate::model::{Dataset, SCHEMA_VERSION};

/// Managed columns injected by the DDL compiler — never set in seed values.
const RESERVED: &[&str] = &["id", "tenant_id"];

fn error(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Issue {
    Issue {
        severity: Severity::Error,
        code,
        path: path.into(),
        message: message.into(),
    }
}

/// Validate `dataset` against `catalog`.
pub fn validate(dataset: &Dataset, catalog: &Catalog) -> Result<(), Vec<Issue>> {
    let mut issues = Vec::new();

    if !schema_version_compatible(&dataset.schema_version) {
        issues.push(error(
            "unsupported-schema-version",
            "schema-version",
            format!(
                "dataset schema-version {:?} is not compatible with {SCHEMA_VERSION}.x",
                dataset.schema_version
            ),
        ));
    }
    if dataset.catalog_id != catalog.catalog_id {
        issues.push(error(
            "catalog-id-mismatch",
            "catalog-id",
            format!(
                "dataset targets catalog {:?} but was validated against {:?}",
                dataset.catalog_id, catalog.catalog_id
            ),
        ));
    }

    // The set of seeded keys per entity id — for reference resolution.
    let mut seeded: HashMap<&str, HashSet<&str>> = HashMap::new();
    for es in &dataset.entities {
        let set = seeded.entry(es.entity.as_str()).or_default();
        for r in &es.rows {
            set.insert(r.key.as_str());
        }
    }

    let mut seen_entities = HashSet::new();
    for (ei, es) in dataset.entities.iter().enumerate() {
        let path = format!("entities[{ei}]");
        if !seen_entities.insert(es.entity.as_str()) {
            issues.push(error(
                "duplicate-entity-seed",
                format!("{path}.entity"),
                format!("entity {:?} is seeded more than once", es.entity),
            ));
        }
        let Some(entity) = catalog.entities.iter().find(|e| e.id == es.entity) else {
            issues.push(error(
                "unknown-entity",
                format!("{path}.entity"),
                format!("no entity {:?} in the catalog", es.entity),
            ));
            continue;
        };

        // field id -> field, and field NAME -> field (values are keyed by name).
        let by_name: HashMap<&str, &wamn_catalog::Field> =
            entity.fields.iter().map(|f| (f.name.as_str(), f)).collect();

        let mut keys = HashSet::new();
        for (ri, row) in es.rows.iter().enumerate() {
            let rpath = format!("{path}.rows[{ri}]");
            if row.key.trim().is_empty() {
                issues.push(error(
                    "empty-key",
                    format!("{rpath}.key"),
                    "row key is empty",
                ));
            } else if !keys.insert(row.key.as_str()) {
                issues.push(error(
                    "duplicate-key",
                    format!("{rpath}.key"),
                    format!(
                        "key {:?} is used by more than one row in {:?}",
                        row.key, es.entity
                    ),
                ));
            }

            for (name, value) in &row.values {
                let vpath = format!("{rpath}.values.{name}");
                if RESERVED.contains(&name.as_str()) {
                    issues.push(error(
                        "reserved-field",
                        &vpath,
                        format!("{name:?} is a managed column and cannot be seeded"),
                    ));
                    continue;
                }
                let Some(field) = by_name.get(name.as_str()) else {
                    issues.push(error(
                        "unknown-field",
                        &vpath,
                        format!("entity {:?} has no field named {name:?}", es.entity),
                    ));
                    continue;
                };
                check_value(&mut issues, &vpath, &field.field_type, value, &seeded);
            }

            // Required (non-nullable, no default, user-supplied) fields present.
            for field in &entity.fields {
                if !field.nullable
                    && field.default.is_none()
                    && !row.values.contains_key(&field.name)
                {
                    issues.push(error(
                        "missing-required-field",
                        format!("{rpath}.values.{}", field.name),
                        format!(
                            "required field {:?} has no value and no default",
                            field.name
                        ),
                    ));
                }
            }
        }

        check_unique_tuples(&mut issues, &path, entity, es);
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

fn check_value(
    issues: &mut Vec<Issue>,
    path: &str,
    ty: &FieldType,
    value: &Value,
    seeded: &HashMap<&str, HashSet<&str>>,
) {
    // A JSON null is always an acceptable value (the column is left NULL / default).
    if value.is_null() {
        return;
    }
    match ty {
        FieldType::Text { max_len } => match value.as_str() {
            None => type_err(issues, path, "text", value),
            Some(s) => {
                if let Some(n) = max_len
                    && s.chars().count() > *n as usize
                {
                    issues.push(error(
                        "text-too-long",
                        path,
                        format!("text is longer than max-len {n}"),
                    ));
                }
            }
        },
        FieldType::Int | FieldType::BigInt => {
            if !(value.is_i64() || value.is_u64()) {
                type_err(issues, path, "integer", value);
            }
        }
        FieldType::Bool => {
            if !value.is_boolean() {
                type_err(issues, path, "bool", value);
            }
        }
        FieldType::Uuid => match value.as_str() {
            Some(s) if uuid::Uuid::parse_str(s).is_ok() => {}
            _ => issues.push(error("invalid-uuid", path, "value is not a uuid string")),
        },
        FieldType::Json => {} // any JSON value is valid
        FieldType::Date | FieldType::Timestamptz => {
            if !value.is_string() {
                type_err(issues, path, "date/timestamptz string", value);
            }
        }
        FieldType::Enum { variants } => match value.as_str() {
            Some(s) if variants.iter().any(|v| v == s) => {}
            Some(s) => issues.push(error(
                "enum-not-a-variant",
                path,
                format!("{s:?} is not one of {variants:?}"),
            )),
            None => type_err(issues, path, "enum string", value),
        },
        FieldType::Numeric {
            precision, scale, ..
        } => check_numeric(issues, path, *precision, *scale, value),
        FieldType::Reference { entity } => match value.as_str() {
            None => type_err(issues, path, "reference key string", value),
            Some(key) => {
                if !seeded
                    .get(entity.as_str())
                    .is_some_and(|ks| ks.contains(key))
                {
                    issues.push(error(
                        "unknown-reference",
                        path,
                        format!("reference {key:?} is not a seeded key of entity {entity:?}"),
                    ));
                }
            }
        },
    }
}

/// Exact-decimal check: floats are rejected outright (the no-float rule); a
/// string decimal or an integer must fit `numeric(precision, scale)`.
fn check_numeric(issues: &mut Vec<Issue>, path: &str, precision: u32, scale: u32, value: &Value) {
    let (int_digits, frac_digits) = match value {
        Value::String(s) => match parse_decimal(s) {
            Some(dd) => dd,
            None => {
                issues.push(error(
                    "numeric-not-exact",
                    path,
                    format!("{s:?} is not a decimal"),
                ));
                return;
            }
        },
        Value::Number(n) if n.is_i64() || n.is_u64() => {
            let digits = n.to_string().trim_start_matches('-').len() as u32;
            (digits, 0)
        }
        Value::Number(_) => {
            issues.push(error(
                "numeric-not-exact",
                path,
                "numeric value must be an exact decimal string or integer, not a float",
            ));
            return;
        }
        _ => {
            type_err(issues, path, "numeric", value);
            return;
        }
    };
    if frac_digits > scale || int_digits > precision.saturating_sub(scale) {
        issues.push(error(
            "numeric-out-of-range",
            path,
            format!("value does not fit numeric({precision},{scale})"),
        ));
    }
}

/// Digit counts `(integer_digits, fractional_digits)` of a plain decimal, or
/// `None` if the string is not a bare decimal. Leading zeros in the integer part
/// are ignored so `0.50` counts as 0 integer digits.
fn parse_decimal(s: &str) -> Option<(u32, u32)> {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let int_digits = int.trim_start_matches('0').len() as u32;
    Some((int_digits, frac.len() as u32))
}

fn check_unique_tuples(
    issues: &mut Vec<Issue>,
    path: &str,
    entity: &Entity,
    es: &crate::model::EntitySeed,
) {
    let id_to_name: HashMap<&str, &str> = entity
        .fields
        .iter()
        .map(|f| (f.id.as_str(), f.name.as_str()))
        .collect();

    // Composite unique constraints and unique indexes are enforced on the tenant-
    // scoped tuple; within one dataset (one tenant), check the field tuple.
    let mut tuples: Vec<(&str, Vec<&str>)> = Vec::new();
    for c in &entity.constraints {
        if let Constraint::Unique { name, fields } = c {
            tuples.push((name, fields.iter().map(|s| s.as_str()).collect()));
        }
    }
    for idx in &entity.indexes {
        if idx.unique {
            tuples.push((&idx.name, idx.fields.iter().map(|s| s.as_str()).collect()));
        }
    }

    for (uname, field_ids) in tuples {
        let mut seen = HashSet::new();
        for (ri, row) in es.rows.iter().enumerate() {
            // Read each field's value by NAME; skip rows missing any tuple member.
            let mut tuple = Vec::with_capacity(field_ids.len());
            let mut complete = true;
            for fid in &field_ids {
                match id_to_name.get(fid).and_then(|n| row.values.get(*n)) {
                    Some(v) if !v.is_null() => tuple.push(v.to_string()),
                    _ => {
                        complete = false;
                        break;
                    }
                }
            }
            if complete && !seen.insert(tuple) {
                issues.push(error(
                    "duplicate-unique",
                    format!("{path}.rows[{ri}]"),
                    format!("row violates unique {uname:?}"),
                ));
            }
        }
    }
}

fn type_err(issues: &mut Vec<Issue>, path: &str, expected: &str, value: &Value) {
    issues.push(error(
        "value-type-mismatch",
        path,
        format!("expected {expected}, got {value}"),
    ));
}

fn schema_version_compatible(v: &str) -> bool {
    fn major_minor(s: &str) -> Option<(u32, u32)> {
        let mut it = s.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next().unwrap_or("0").parse().ok()?;
        Some((major, minor))
    }
    match (major_minor(v), major_minor(SCHEMA_VERSION)) {
        (Some((vmaj, vmin)), Some((cmaj, cmin))) => vmaj == cmaj && vmin <= cmin,
        _ => false,
    }
}
