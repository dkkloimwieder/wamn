//! Compile a [`Dataset`] into tenant-scoped, idempotent `INSERT`s.
//!
//! Entities are emitted in **foreign-key-safe order** (a referencing entity after
//! the entities it points at), one `INSERT` per row. Each row gets its
//! deterministic managed `id` (so re-seeding is stable), the target `tenant`,
//! and `ON CONFLICT (id) DO NOTHING` so a repeated load — a test host cloning a
//! schema, a re-seed — is a no-op on existing rows. Output is a [`MigrationPlan`]
//! (reused from 3.2); every operation is additive.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use wamn_catalog::{Catalog, Entity, FieldType, Issue};
use wamn_ddl::sql::{quote_ident, quote_literal};
use wamn_ddl::{MigrationPlan, Operation, Safety};

use crate::id::row_id;
use crate::model::{Dataset, EntitySeed};
use crate::validate;

/// Why a dataset could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// The dataset failed structural validation against the catalog.
    InvalidDataset(Vec<Issue>),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::InvalidDataset(issues) => {
                write!(f, "seed dataset is invalid ({} error(s)): ", issues.len())?;
                for (i, issue) in issues.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{issue}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// Compile `dataset` into an idempotent seed-load plan for `tenant`. Validates
/// first (an invalid dataset is rejected, not compiled to broken SQL).
pub fn compile(
    dataset: &Dataset,
    catalog: &Catalog,
    tenant: &str,
) -> Result<MigrationPlan, CompileError> {
    validate::validate(dataset, catalog).map_err(CompileError::InvalidDataset)?;

    let mut operations = Vec::new();
    for es in fk_safe_order(dataset, catalog) {
        let entity = catalog
            .entities
            .iter()
            .find(|e| e.id == es.entity)
            .expect("validated: entity resolves");
        for row in &es.rows {
            operations.push(row_insert(entity, es, row, tenant));
        }
    }
    Ok(MigrationPlan { operations })
}

/// One `INSERT … ON CONFLICT (id) DO NOTHING` for a seed row.
fn row_insert(
    entity: &Entity,
    es: &EntitySeed,
    row: &crate::model::SeedRow,
    tenant: &str,
) -> Operation {
    let by_name: HashMap<&str, &FieldType> = entity
        .fields
        .iter()
        .map(|f| (f.name.as_str(), &f.field_type))
        .collect();

    let mut cols = vec!["id".to_string(), "tenant_id".to_string()];
    let mut vals = vec![
        quote_literal(&row_id(tenant, &es.entity, &row.key).to_string()),
        quote_literal(tenant),
    ];
    // BTreeMap iteration is sorted → stable column order.
    for (name, value) in &row.values {
        let ty = by_name
            .get(name.as_str())
            .expect("validated: field resolves");
        cols.push(quote_ident(name));
        vals.push(render(ty, value, tenant));
    }

    let sql = format!(
        "INSERT INTO {tbl} ({cols})\n    VALUES ({vals})\n    ON CONFLICT (id) DO NOTHING",
        tbl = quote_ident(&entity.name),
        cols = cols.join(", "),
        vals = vals.join(", "),
    );
    Operation {
        summary: format!("seed {}.{}", es.entity, row.key),
        sql,
        safety: Safety::Additive,
        entity: entity.id.clone(),
        field: None,
        note: None,
    }
}

/// Render a JSON value as a SQL literal for a field of type `ty`.
fn render(ty: &FieldType, value: &Value, tenant: &str) -> String {
    if value.is_null() {
        return "NULL".to_string();
    }
    match ty {
        FieldType::Text { .. }
        | FieldType::Uuid
        | FieldType::Date
        | FieldType::Timestamptz
        | FieldType::Enum { .. } => quote_literal(value.as_str().unwrap_or_default()),
        FieldType::Int | FieldType::BigInt => value.to_string(),
        FieldType::Bool => value.as_bool().unwrap_or(false).to_string(),
        FieldType::Json => format!("{}::jsonb", quote_literal(&value.to_string())),
        // Exact-decimal literal, unquoted (validation guarantees it is one).
        FieldType::Numeric { .. } => match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        },
        // A reference resolves to the target row's deterministic id.
        FieldType::Reference { entity } => {
            let key = value.as_str().unwrap_or_default();
            quote_literal(&row_id(tenant, entity, key).to_string())
        }
    }
}

/// The dataset's entities in foreign-key-safe order: an entity is emitted after
/// every *other* dataset entity it references. Self-references are handled by
/// per-row author order. A reference cycle falls back to author order.
fn fk_safe_order<'a>(dataset: &'a Dataset, catalog: &Catalog) -> Vec<&'a EntitySeed> {
    let present: HashSet<&str> = dataset.entities.iter().map(|e| e.entity.as_str()).collect();

    // deps[E] = the other seeded entities E references.
    let mut deps: HashMap<&str, HashSet<&str>> = HashMap::new();
    for es in &dataset.entities {
        let set = deps.entry(es.entity.as_str()).or_default();
        if let Some(entity) = catalog.entities.iter().find(|e| e.id == es.entity) {
            for f in &entity.fields {
                if let FieldType::Reference { entity: target } = &f.field_type
                    && target != &es.entity
                    && present.contains(target.as_str())
                {
                    set.insert(target.as_str());
                }
            }
        }
    }

    // Kahn's algorithm, breaking ties by original order for determinism.
    let mut ordered: Vec<&str> = Vec::new();
    let mut placed: HashSet<&str> = HashSet::new();
    while ordered.len() < dataset.entities.len() {
        let mut progressed = false;
        for es in &dataset.entities {
            let e = es.entity.as_str();
            if placed.contains(e) {
                continue;
            }
            let ready = deps[e].iter().all(|d| placed.contains(d));
            if ready {
                ordered.push(e);
                placed.insert(e);
                progressed = true;
            }
        }
        if !progressed {
            // Cycle: append the rest in author order.
            for es in &dataset.entities {
                if placed.insert(es.entity.as_str()) {
                    ordered.push(es.entity.as_str());
                }
            }
            break;
        }
    }

    ordered
        .iter()
        .map(|e| dataset.entities.iter().find(|es| es.entity == *e).unwrap())
        .collect()
}
