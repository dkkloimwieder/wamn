//! Postgres DDL emission from the catalog model.
//!
//! Generated tables carry the platform multi-tenancy floor: a managed
//! `id uuid` primary key, a `tenant_id` column, `FORCE ROW LEVEL SECURITY`, and
//! the `app.tenant` tenant policy (the S2 / 2.2 shape). Uniqueness and indexes
//! are tenant-scoped (prefixed with `tenant_id`). Per-role row rules are layered
//! on later by the RLS policy builder (3.5).

use std::collections::HashMap;

use serde_json::Value;
use wamn_catalog::{Catalog, Constraint, Entity, FieldType, Index};

use crate::plan::{MigrationPlan, Operation, Safety};

/// The managed columns every generated table carries. A user field may not reuse
/// these names.
pub(crate) const RESERVED_COLUMNS: &[&str] = &["id", "tenant_id"];

/// Quote a SQL identifier. Delegates to the shared [`crate::sql`] helpers so DDL
/// and RLS-policy emission (3.5) quote identically.
fn q(ident: &str) -> String {
    crate::sql::quote_ident(ident)
}

/// Quote a SQL string literal.
fn lit(s: &str) -> String {
    crate::sql::quote_literal(s)
}

/// The Postgres column type for a field type. A `reference` is stored as the
/// referenced entity's managed `uuid` primary key; the foreign key itself is a
/// separate constraint.
fn sql_type(ty: &FieldType) -> String {
    match ty {
        FieldType::Text { max_len: Some(n) } => format!("varchar({n})"),
        FieldType::Text { max_len: None } => "text".into(),
        FieldType::Int => "integer".into(),
        FieldType::BigInt => "bigint".into(),
        FieldType::Bool => "boolean".into(),
        FieldType::Uuid => "uuid".into(),
        FieldType::Json => "jsonb".into(),
        FieldType::Date => "date".into(),
        FieldType::Timestamptz => "timestamptz".into(),
        FieldType::Enum { .. } => "text".into(),
        FieldType::Numeric {
            precision, scale, ..
        } => format!("numeric({precision},{scale})"),
        FieldType::Reference { .. } => "uuid".into(),
    }
}

/// Render a JSON default as a SQL default literal. Expression defaults are not
/// supported in 0.1 (a 0.2 item) — a string default is a quoted literal.
fn default_literal(v: &Value) -> String {
    match v {
        Value::String(s) => lit(s),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "NULL".into(),
        other => format!("{}::jsonb", lit(&other.to_string())),
    }
}

/// The column clause for a field, used by both `CREATE TABLE` and `ADD COLUMN`:
/// `"name" <type> [NOT NULL] [DEFAULT ...] [CHECK (... IN (...))]`.
fn column_clause(name: &str, field: &wamn_catalog::Field) -> String {
    let mut c = format!("{} {}", q(name), sql_type(&field.field_type));
    if !field.nullable {
        c.push_str(" NOT NULL");
    }
    if let Some(d) = &field.default {
        c.push_str(&format!(" DEFAULT {}", default_literal(d)));
    }
    if let FieldType::Enum { variants } = &field.field_type {
        let list = variants
            .iter()
            .map(|v| lit(v))
            .collect::<Vec<_>>()
            .join(", ");
        c.push_str(&format!(" CHECK ({} IN ({list}))", q(name)));
    }
    c
}

/// Resolve field-id -> physical name within an entity.
fn field_names(entity: &Entity) -> HashMap<&str, &str> {
    entity
        .fields
        .iter()
        .map(|f| (f.id.as_str(), f.name.as_str()))
        .collect()
}

/// Resolve entity-id -> physical name across a catalog.
fn entity_names(catalog: &Catalog) -> HashMap<&str, &str> {
    catalog
        .entities
        .iter()
        .map(|e| (e.id.as_str(), e.name.as_str()))
        .collect()
}

/// `CREATE TABLE` with the managed columns + user columns.
fn create_table_op(entity: &Entity) -> Operation {
    let t = &entity.name;
    let mut cols = vec![
        "id uuid PRIMARY KEY DEFAULT gen_random_uuid()".to_string(),
        "tenant_id text NOT NULL".to_string(),
    ];
    for f in &entity.fields {
        cols.push(column_clause(&f.name, f));
    }
    let sql = format!("CREATE TABLE {} (\n    {}\n)", q(t), cols.join(",\n    "));
    Operation {
        summary: format!("create table {t}"),
        sql,
        safety: Safety::Additive,
        entity: entity.id.clone(),
        field: None,
        note: None,
    }
}

/// The RLS floor for a table: enable + force RLS, the tenant policy, and the
/// wamn_app grant — one multi-statement operation.
fn rls_op(entity: &Entity) -> Operation {
    let t = &entity.name;
    let policy = format!("{t}_tenant");
    let sql = format!(
        "ALTER TABLE {tbl} ENABLE ROW LEVEL SECURITY;\n\
         ALTER TABLE {tbl} FORCE ROW LEVEL SECURITY;\n\
         CREATE POLICY {pol} ON {tbl}\n    \
         USING (tenant_id = current_setting('app.tenant', true))\n    \
         WITH CHECK (tenant_id = current_setting('app.tenant', true));\n\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {tbl} TO wamn_app",
        tbl = q(t),
        pol = q(&policy),
    );
    Operation {
        summary: format!("enable tenant RLS on {t}"),
        sql,
        safety: Safety::Additive,
        entity: entity.id.clone(),
        field: None,
        note: None,
    }
}

/// Foreign-key constraint for a `reference` field.
fn fk_op(entity: &Entity, field: &wamn_catalog::Field, target_table: &str) -> Operation {
    let t = &entity.name;
    let cname = format!("{t}_{}_fkey", field.name);
    let sql = format!(
        "ALTER TABLE {tbl} ADD CONSTRAINT {c} FOREIGN KEY ({col}) REFERENCES {tgt} (id)",
        tbl = q(t),
        c = q(&cname),
        col = q(&field.name),
        tgt = q(target_table),
    );
    Operation {
        summary: format!("add foreign key {t}.{} -> {target_table}", field.name),
        sql,
        safety: Safety::Additive,
        entity: entity.id.clone(),
        field: Some(field.id.clone()),
        note: None,
    }
}

/// A `UNIQUE` / `CHECK` table constraint (tenant-scoped for uniqueness).
fn constraint_op(entity: &Entity, c: &Constraint) -> Operation {
    let t = &entity.name;
    let fnames = field_names(entity);
    let (sql, summary) = match c {
        Constraint::Unique { name, fields } => {
            let cols = std::iter::once("tenant_id".to_string())
                .chain(
                    fields
                        .iter()
                        .map(|fid| q(fnames.get(fid.as_str()).copied().unwrap_or(fid))),
                )
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!(
                    "ALTER TABLE {tbl} ADD CONSTRAINT {c} UNIQUE ({cols})",
                    tbl = q(t),
                    c = q(name),
                ),
                format!("add unique constraint {name} on {t}"),
            )
        }
        Constraint::Check { name, expression } => (
            format!(
                "ALTER TABLE {tbl} ADD CONSTRAINT {c} CHECK ({expression})",
                tbl = q(t),
                c = q(name),
            ),
            format!("add check constraint {name} on {t}"),
        ),
    };
    Operation {
        summary,
        sql,
        // Adding a constraint can fail on pre-existing violating rows, but it is
        // not data-losing — additive with a caveat.
        safety: Safety::Additive,
        entity: entity.id.clone(),
        field: None,
        note: Some("fails if existing rows violate the constraint".into()),
    }
}

/// A secondary index (tenant-scoped).
fn index_op(entity: &Entity, idx: &Index) -> Operation {
    let t = &entity.name;
    let fnames = field_names(entity);
    let cols = std::iter::once("tenant_id".to_string())
        .chain(
            idx.fields
                .iter()
                .map(|fid| q(fnames.get(fid.as_str()).copied().unwrap_or(fid))),
        )
        .collect::<Vec<_>>()
        .join(", ");
    let unique = if idx.unique { "UNIQUE " } else { "" };
    let sql = format!(
        "CREATE {unique}INDEX {name} ON {tbl} ({cols})",
        name = q(&idx.name),
        tbl = q(t),
    );
    Operation {
        summary: format!(
            "create {}index {} on {t}",
            if idx.unique { "unique " } else { "" },
            idx.name
        ),
        sql,
        safety: Safety::Additive,
        entity: entity.id.clone(),
        field: None,
        note: None,
    }
}

/// A column comment carrying a numeric field's unit (metadata that survives to
/// the DB).
fn unit_comment_op(entity: &Entity, field: &wamn_catalog::Field, unit: &str) -> Operation {
    let t = &entity.name;
    let sql = format!(
        "COMMENT ON COLUMN {tbl}.{col} IS {c}",
        tbl = q(t),
        col = q(&field.name),
        c = lit(&format!("unit: {unit}")),
    );
    Operation {
        summary: format!("comment unit on {t}.{}", field.name),
        sql,
        safety: Safety::Additive,
        entity: entity.id.clone(),
        field: Some(field.id.clone()),
        note: None,
    }
}

/// FKs, constraints, indexes, unit comments for an entity (everything that must
/// come after every table exists).
fn emit_entity_attachments(plan: &mut MigrationPlan, entity: &Entity, names: &HashMap<&str, &str>) {
    for f in &entity.fields {
        if let FieldType::Reference { entity: target } = &f.field_type {
            let target_table = names.get(target.as_str()).copied().unwrap_or(target);
            plan.push(fk_op(entity, f, target_table));
        }
        if let FieldType::Numeric { unit: Some(u), .. } = &f.field_type {
            plan.push(unit_comment_op(entity, f, u));
        }
    }
    for c in &entity.constraints {
        plan.push(constraint_op(entity, c));
    }
    for idx in &entity.indexes {
        plan.push(index_op(entity, idx));
    }
}

/// Full CREATE plan for a catalog: all tables + RLS first, then all attachments
/// (so foreign keys never precede their target table).
pub(crate) fn create_plan(catalog: &Catalog) -> MigrationPlan {
    let names = entity_names(catalog);
    let mut plan = MigrationPlan::default();
    for e in &catalog.entities {
        plan.push(create_table_op(e));
        plan.push(rls_op(e));
    }
    for e in &catalog.entities {
        emit_entity_attachments(&mut plan, e, &names);
    }
    plan
}

/// Migration plan from `old` -> `new`, driven by the catalog diff.
pub(crate) fn migrate_plan(old: &Catalog, new: &Catalog) -> MigrationPlan {
    let names = entity_names(new);
    let old_by_id: HashMap<&str, &Entity> =
        old.entities.iter().map(|e| (e.id.as_str(), e)).collect();
    let new_by_id: HashMap<&str, &Entity> =
        new.entities.iter().map(|e| (e.id.as_str(), e)).collect();

    let d = wamn_catalog::diff(old, new);
    let mut plan = MigrationPlan::default();

    // 1) New entities (tables first, then all their attachments).
    for id in &d.entities_added {
        let e = new_by_id[id.as_str()];
        plan.push(create_table_op(e));
        plan.push(rls_op(e));
    }
    for id in &d.entities_added {
        emit_entity_attachments(&mut plan, new_by_id[id.as_str()], &names);
    }

    // 2) Changed entities — additive column/index/constraint adds first.
    for ch in &d.entities_changed {
        let old_e = old_by_id[ch.id.as_str()];
        let new_e = new_by_id[ch.id.as_str()];
        emit_additive_changes(&mut plan, old_e, new_e, ch, &names);
    }

    // 3) Column alters (rename / retype / nullability / default).
    for ch in &d.entities_changed {
        let new_e = new_by_id[ch.id.as_str()];
        emit_column_alters(&mut plan, new_e, ch);
    }

    // 4) Destructive removals — drop constraints/indexes, then columns, then
    //    (last) tables, so dependencies unwind cleanly.
    for ch in &d.entities_changed {
        let old_e = old_by_id[ch.id.as_str()];
        let new_e = new_by_id[ch.id.as_str()];
        emit_destructive_changes(&mut plan, old_e, new_e, ch);
    }
    for id in &d.entities_removed {
        let e = old_by_id[id.as_str()];
        plan.push(Operation {
            summary: format!("drop table {}", e.name),
            sql: format!("DROP TABLE {}", q(&e.name)),
            safety: Safety::Destructive,
            entity: e.id.clone(),
            field: None,
            note: Some("drops the table and all its data".into()),
        });
    }

    plan
}

fn field_by_id<'a>(entity: &'a Entity, id: &str) -> Option<&'a wamn_catalog::Field> {
    entity.fields.iter().find(|f| f.id == id)
}

/// Added columns (+ their FK / unit comment), added constraints, added indexes.
fn emit_additive_changes(
    plan: &mut MigrationPlan,
    old_e: &Entity,
    new_e: &Entity,
    ch: &wamn_catalog::EntityChange,
    names: &HashMap<&str, &str>,
) {
    for fid in &ch.fields_added {
        let f = field_by_id(new_e, fid).expect("added field exists in new");
        let note = (!f.nullable && f.default.is_none())
            .then(|| "NOT NULL with no default fails on a non-empty table".to_string());
        plan.push(Operation {
            summary: format!("add column {}.{}", new_e.name, f.name),
            sql: format!(
                "ALTER TABLE {} ADD COLUMN {}",
                q(&new_e.name),
                column_clause(&f.name, f)
            ),
            safety: Safety::Additive,
            entity: new_e.id.clone(),
            field: Some(f.id.clone()),
            note,
        });
        if let FieldType::Reference { entity: target } = &f.field_type {
            let target_table = names.get(target.as_str()).copied().unwrap_or(target);
            plan.push(fk_op(new_e, f, target_table));
        }
        if let FieldType::Numeric { unit: Some(u), .. } = &f.field_type {
            plan.push(unit_comment_op(new_e, f, u));
        }
    }

    // Added constraints / indexes (set difference by identity).
    for c in &new_e.constraints {
        if !old_e.constraints.iter().any(|o| o == c) {
            plan.push(constraint_op(new_e, c));
        }
    }
    for idx in &new_e.indexes {
        if !old_e.indexes.iter().any(|o| o == idx) {
            plan.push(index_op(new_e, idx));
        }
    }
}

/// Column renames, retypes, nullability, and default changes.
fn emit_column_alters(plan: &mut MigrationPlan, new_e: &Entity, ch: &wamn_catalog::EntityChange) {
    for fc in &ch.fields_changed {
        let f = field_by_id(new_e, &fc.id).expect("changed field exists in new");
        if let Some((from, to)) = &fc.name_changed {
            plan.push(Operation {
                summary: format!("rename column {}.{from} -> {to}", new_e.name),
                sql: format!(
                    "ALTER TABLE {} RENAME COLUMN {} TO {}",
                    q(&new_e.name),
                    q(from),
                    q(to)
                ),
                safety: Safety::Destructive,
                entity: new_e.id.clone(),
                field: Some(fc.id.clone()),
                note: Some("breaks generated API / flows referencing the old name".into()),
            });
        }
        if fc.type_changed.is_some() {
            let ty = sql_type(&f.field_type);
            plan.push(Operation {
                summary: format!("retype column {}.{}", new_e.name, f.name),
                sql: format!(
                    "ALTER TABLE {tbl} ALTER COLUMN {col} TYPE {ty} USING {col}::{ty}",
                    tbl = q(&new_e.name),
                    col = q(&f.name),
                ),
                safety: Safety::Destructive,
                entity: new_e.id.clone(),
                field: Some(fc.id.clone()),
                note: Some("cast may fail or truncate existing values".into()),
            });
        }
        if fc.nullable_changed {
            let (verb, safety, note) = if f.nullable {
                ("DROP NOT NULL", Safety::Additive, None)
            } else {
                (
                    "SET NOT NULL",
                    Safety::Destructive,
                    Some("fails if existing rows hold NULLs".to_string()),
                )
            };
            plan.push(Operation {
                summary: format!("alter {}.{} {}", new_e.name, f.name, verb.to_lowercase()),
                sql: format!(
                    "ALTER TABLE {} ALTER COLUMN {} {verb}",
                    q(&new_e.name),
                    q(&f.name)
                ),
                safety,
                entity: new_e.id.clone(),
                field: Some(fc.id.clone()),
                note,
            });
        }
        if fc.default_changed {
            let clause = match &f.default {
                Some(d) => format!("SET DEFAULT {}", default_literal(d)),
                None => "DROP DEFAULT".to_string(),
            };
            plan.push(Operation {
                summary: format!("alter default {}.{}", new_e.name, f.name),
                sql: format!(
                    "ALTER TABLE {} ALTER COLUMN {} {clause}",
                    q(&new_e.name),
                    q(&f.name)
                ),
                safety: Safety::Additive,
                entity: new_e.id.clone(),
                field: Some(fc.id.clone()),
                note: None,
            });
        }
    }
}

/// Dropped columns, dropped indexes, dropped constraints, table renames.
fn emit_destructive_changes(
    plan: &mut MigrationPlan,
    old_e: &Entity,
    new_e: &Entity,
    ch: &wamn_catalog::EntityChange,
) {
    // Dropped constraints (removes a guarantee — needs review).
    for c in &old_e.constraints {
        if !new_e.constraints.iter().any(|n| n == c) {
            plan.push(Operation {
                summary: format!("drop constraint {} on {}", c.name(), new_e.name),
                sql: format!(
                    "ALTER TABLE {} DROP CONSTRAINT {}",
                    q(&new_e.name),
                    q(c.name())
                ),
                safety: Safety::Destructive,
                entity: new_e.id.clone(),
                field: None,
                note: Some("removes a data-integrity guarantee".into()),
            });
        }
    }
    // Dropped indexes: a unique index drop relaxes a guarantee (destructive); a
    // plain index drop is data-safe (additive).
    for idx in &old_e.indexes {
        if !new_e.indexes.iter().any(|n| n == idx) {
            plan.push(Operation {
                summary: format!("drop index {}", idx.name),
                sql: format!("DROP INDEX {}", q(&idx.name)),
                safety: if idx.unique {
                    Safety::Destructive
                } else {
                    Safety::Additive
                },
                entity: new_e.id.clone(),
                field: None,
                note: idx.unique.then(|| "removes a uniqueness guarantee".into()),
            });
        }
    }
    // Dropped columns.
    for fid in &ch.fields_removed {
        let f = field_by_id(old_e, fid).expect("removed field exists in old");
        plan.push(Operation {
            summary: format!("drop column {}.{}", new_e.name, f.name),
            sql: format!("ALTER TABLE {} DROP COLUMN {}", q(&new_e.name), q(&f.name)),
            safety: Safety::Destructive,
            entity: new_e.id.clone(),
            field: Some(f.id.clone()),
            note: Some("drops the column and its data".into()),
        });
    }
    // Table rename.
    if old_e.name != new_e.name {
        plan.push(Operation {
            summary: format!("rename table {} -> {}", old_e.name, new_e.name),
            sql: format!(
                "ALTER TABLE {} RENAME TO {}",
                q(&old_e.name),
                q(&new_e.name)
            ),
            safety: Safety::Destructive,
            entity: new_e.id.clone(),
            field: None,
            note: Some("breaks generated API / flows referencing the old name".into()),
        });
    }
}
