//! Postgres DDL emission from the catalog model.
//!
//! Generated tables carry the platform multi-tenancy floor: a managed
//! `id uuid` primary key, a `tenant_id` column, `FORCE ROW LEVEL SECURITY`, and
//! the `app.tenant` tenant policy (the S2 / 2.2 shape). Uniqueness and indexes
//! are tenant-scoped (prefixed with `tenant_id`). Per-role row rules are layered
//! on later by the RLS policy builder (3.5).

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use wamn_catalog::{Catalog, Constraint, Entity, FieldType, Index};

use crate::CompileError;
use crate::plan::{MigrationPlan, Operation, Safety};

/// The managed columns every generated table carries. A user field may not reuse
/// these names.
pub(crate) const RESERVED_COLUMNS: &[&str] = &["id", "tenant_id"];

/// Transient-name prefix for a dropped table (and its indexes) renamed aside
/// because the name is reclaimed in the same migration (see [`migrate_plan`]).
pub(crate) const TEMP_DROP_PREFIX: &str = "wamn_mig_drop_";

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

/// A `CHECK` forbidding the JSON-type-changing special values a numeric or
/// timestamp column can otherwise hold. JSON projections of a row (`to_jsonb`,
/// a CDC event payload) serialize `'NaN'::numeric`
/// and `'infinity'::timestamptz`/`date` as JSON **strings** (`"NaN"`,
/// `"infinity"`), so a row-event payload's field would silently change
/// JSON type from number/instant to string; a consumer branching on
/// `jsonb_typeof` mishandles it with no error. Forbidding the values at the
/// column keeps every numeric/timestamp payload field a JSON number/string-
/// instant. Returns `None` for field types that cannot hold such a value.
///
/// - `numeric`: `NaN` is accepted by `numeric(p,s)` (only via flow-authored
///   SQL — the 4.1 gateway already blocks it). PG `NaN = NaN` is TRUE, so
///   `col <> 'NaN'::numeric` is FALSE for `NaN` and rejects it; `Infinity` is
///   already rejected by the precision constraint.
/// - `date`/`timestamptz`: `+/-infinity` are valid instants; forbid both.
///   (`'NaN'::timestamptz` is invalid in PG, so it never reaches `to_jsonb`.)
fn finite_check(name: &str, ty: &FieldType) -> Option<String> {
    let col = q(name);
    match ty {
        FieldType::Numeric { .. } => Some(format!("CHECK ({col} <> 'NaN'::numeric)")),
        FieldType::Date => Some(format!(
            "CHECK ({col} <> 'infinity'::date AND {col} <> '-infinity'::date)"
        )),
        FieldType::Timestamptz => Some(format!(
            "CHECK ({col} <> 'infinity'::timestamptz AND {col} <> '-infinity'::timestamptz)"
        )),
        _ => None,
    }
}

/// The column clause for a field, used by both `CREATE TABLE` and `ADD COLUMN`:
/// `"name" <type> [NOT NULL] [DEFAULT ...] [CHECK (...)]`.
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
    // Exclude the JSON-type-changing special values from numeric/timestamp
    // columns (a field is exactly one type, so this never collides with the
    // enum CHECK above).
    if let Some(check) = finite_check(name, &field.field_type) {
        c.push(' ');
        c.push_str(&check);
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
        // `CHECK (tenant_id <> '')`: Postgres resets a custom GUC to the empty
        // string (not NULL) once SET LOCAL scope ends, so `''` is the value an
        // idle claimless pooled connection carries. The policy below reads it
        // through NULLIF (=> NULL => matches no row); this CHECK makes the
        // guarantee structural by forbidding a `''`-tenant row from ever
        // existing (a superuser/BYPASSRLS write path could otherwise land one).
        "tenant_id text NOT NULL CHECK (tenant_id <> '')".to_string(),
    ];
    for f in &entity.fields {
        cols.push(column_clause(&f.name, f));
    }
    let sql = format!("CREATE TABLE {} (\n    {}\n)", q(t), cols.join(",\n    "));
    Operation {
        summary: format!("create table {t}"),
        sql,
        safety: Safety::Additive,
        entity: entity.id.to_string(),
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
        // NULLIF(..., '') so an empty `app.tenant` claim reads as NULL and
        // matches no row — including a hypothetical `''`-tenant row (see the
        // CHECK (tenant_id <> '') in create_table_op). Bare current_setting
        // would let `''` match a `''`-tenant row that a superuser write left.
        "ALTER TABLE {tbl} ENABLE ROW LEVEL SECURITY;\n\
         ALTER TABLE {tbl} FORCE ROW LEVEL SECURITY;\n\
         CREATE POLICY {pol} ON {tbl}\n    \
         USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))\n    \
         WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));\n\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {tbl} TO wamn_app",
        tbl = q(t),
        pol = q(&policy),
    );
    Operation {
        summary: format!("enable tenant RLS on {t}"),
        sql,
        safety: Safety::Additive,
        entity: entity.id.to_string(),
        field: None,
        note: None,
    }
}

/// The synthesized foreign-key constraint name for a reference field:
/// `<table>_<field>_fkey`. A reference FK is emitted by [`fk_op`] and is never
/// modeled as a catalog constraint, so the sites that add and drop it must
/// agree on this one derivation.
fn fk_constraint_name(table: &str, field_name: &str) -> String {
    format!("{table}_{field_name}_fkey")
}

/// Foreign-key constraint for a `reference` field.
fn fk_op(entity: &Entity, field: &wamn_catalog::Field, target_table: &str) -> Operation {
    let t = &entity.name;
    let cname = fk_constraint_name(t, &field.name);
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
        entity: entity.id.to_string(),
        field: Some(field.id.to_string()),
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
        entity: entity.id.to_string(),
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
        entity: entity.id.to_string(),
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
        entity: entity.id.to_string(),
        field: Some(field.id.to_string()),
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
///
/// Operations are additive-first / destructive-last, EXCEPT the name-freeing
/// preamble. Postgres tables, indexes, and unique-constraint backing indexes
/// share ONE relation namespace, and one migration may both free a name
/// (rename / drop) and reclaim it (create / add) — the freeing side must run
/// first or the add fails (42P07/42710) and the whole transactional apply
/// (2.5) rolls back. Plan order:
///
/// 1. dropped tables whose name — or one of whose index / unique-constraint
///    names — is reclaimed are renamed aside ([`TEMP_DROP_PREFIX`]); their
///    `DROP TABLE` stays last, so FK unwind order is intact (foreign keys
///    follow a rename);
/// 2. dropped indexes and dropped constraints with reclaimed names. Both are
///    pure name-freeing: `DROP INDEX` references no table name, and a
///    constraint drop references its table by the PRE-rename name — a
///    rename's own target may be freed by exactly such a drop, so neither
///    may wait for the renames. An entity that hoists a column drop (step 4)
///    hoists ALL its constraint/index drops here: the hoisted `DROP COLUMN`
///    implicitly drops objects involving the column, and a tail drop would
///    then fail on the missing object;
/// 3. ALL table renames, dependency-ordered (a rename claiming a name freed
///    by another rename waits for it; a cycle — a swap — is rejected), each
///    followed by its implicit `<table>_pkey` rename: index names do not
///    follow a table rename, and letting the pkey name go stale both drifts
///    the recreated table's pkey (Postgres silently suffixes) and lets a
///    LATER migration's aside-rename grab a live table's index. Hoisting
///    every rename also keeps this plan's later `ALTER TABLE <new name>`
///    operations on the same entity valid;
/// 4. per-entity column-namespace freeing: dropped columns whose name is
///    re-added drop now, and ALL column renames run here, dependency-ordered
///    within their table (a swap cycle is rejected). Columns are
///    table-scoped, but the same free-before-claim rule applies (42701
///    otherwise). These reference the table post-rename, hence after step 3;
/// 5. the additive-first / destructive-last steps as before.
///
/// Plans that free no reused name and rename nothing are byte-identical to
/// the preamble-free order.
pub(crate) fn migrate_plan(old: &Catalog, new: &Catalog) -> Result<MigrationPlan, CompileError> {
    let names = entity_names(new);
    let old_by_id: HashMap<&str, &Entity> =
        old.entities.iter().map(|e| (e.id.as_str(), e)).collect();
    let new_by_id: HashMap<&str, &Entity> =
        new.entities.iter().map(|e| (e.id.as_str(), e)).collect();

    let d = wamn_catalog::diff(old, new);
    let mut plan = MigrationPlan::default();

    // ---- Name-reuse analysis: relation names claimed by this plan's adds.
    let mut claimed: HashSet<&str> = HashSet::new();
    for id in &d.entities_added {
        let e = new_by_id[id.as_str()];
        claimed.insert(e.name.as_str());
        claimed.extend(e.indexes.iter().map(|i| i.name.as_str()));
        claimed.extend(e.constraints.iter().filter_map(unique_constraint_name));
    }
    let renames: Vec<(&Entity, &Entity)> = d
        .entities_changed
        .iter()
        .map(|ch| (old_by_id[ch.id.as_str()], new_by_id[ch.id.as_str()]))
        .filter(|(o, n)| o.name != n.name)
        .collect();
    claimed.extend(renames.iter().map(|(_, n)| n.name.as_str()));
    for ch in &d.entities_changed {
        let old_e = old_by_id[ch.id.as_str()];
        let new_e = new_by_id[ch.id.as_str()];
        claimed.extend(
            new_e
                .indexes
                .iter()
                .filter(|i| !old_e.indexes.iter().any(|o| o == *i))
                .map(|i| i.name.as_str()),
        );
        claimed.extend(
            new_e
                .constraints
                .iter()
                .filter(|c| !old_e.constraints.iter().any(|o| o == *c))
                .filter_map(unique_constraint_name),
        );
    }

    // ---- Preamble 1: rename doomed tables (and their indexes — index names
    // do NOT follow a table rename) aside where a name is reclaimed. Every
    // synthesized aside name must be free in the SHARED relation namespace
    // (tables, indexes, unique-constraint backing indexes, implicit pkeys),
    // checked conservatively against both catalog versions.
    let all_relnames: HashSet<String> = old
        .entities
        .iter()
        .chain(new.entities.iter())
        .flat_map(|e| {
            std::iter::once(e.name.clone())
                .chain(std::iter::once(format!("{}_pkey", e.name)))
                .chain(e.indexes.iter().map(|i| i.name.clone()))
                .chain(
                    e.constraints
                        .iter()
                        .filter_map(unique_constraint_name)
                        .map(str::to_string),
                )
        })
        .collect();
    let mut temp_named: HashMap<&str, String> = HashMap::new();
    for id in &d.entities_removed {
        let e = old_by_id[id.as_str()];
        let table_reclaimed = claimed.contains(e.name.as_str());
        // Index names that must move aside with (or without) the table: when
        // the TABLE name is reclaimed, ALL of them — the recreated table's
        // indexes (including its implicit `<table>_pkey`) must get their
        // canonical names instead of colliding or silently drifting; when
        // only an index name is reclaimed, just those.
        let index_asides: Vec<String> = if table_reclaimed {
            std::iter::once(format!("{}_pkey", e.name))
                .chain(e.indexes.iter().map(|i| i.name.clone()))
                .chain(
                    e.constraints
                        .iter()
                        .filter_map(unique_constraint_name)
                        .map(str::to_string),
                )
                .collect()
        } else {
            e.indexes
                .iter()
                .map(|i| i.name.as_str())
                .chain(e.constraints.iter().filter_map(unique_constraint_name))
                .filter(|n| claimed.contains(*n))
                .map(str::to_string)
                .collect()
        };
        let mut aside_sources: Vec<&str> = Vec::new();
        if table_reclaimed {
            aside_sources.push(e.name.as_str());
        }
        aside_sources.extend(index_asides.iter().map(String::as_str));
        for src in &aside_sources {
            let tmp = format!("{TEMP_DROP_PREFIX}{src}");
            if all_relnames.contains(&tmp) {
                return Err(CompileError::TempNameCollision { name: tmp });
            }
        }
        if table_reclaimed {
            let tmp = format!("{TEMP_DROP_PREFIX}{}", e.name);
            plan.push(temp_rename_table_op(e, &tmp));
            temp_named.insert(e.id.as_str(), tmp);
        }
        for n in &index_asides {
            plan.push(temp_rename_index_op(e, n));
        }
    }

    // ---- Column-namespace analysis (emitted in preamble 4 below, but the
    // partition in preamble 2 needs to know which entities hoist a column
    // drop): a dropped column whose name is re-added — or claimed by a column
    // rename — must drop before the claims.
    let mut col_renames_by: HashMap<&str, Vec<(&str, &str, &str)>> = HashMap::new();
    let mut hoisted_columns: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut hoisted_col_fields: HashMap<&str, Vec<&wamn_catalog::Field>> = HashMap::new();
    for ch in &d.entities_changed {
        let old_e = old_by_id[ch.id.as_str()];
        let new_e = new_by_id[ch.id.as_str()];
        let col_renames: Vec<(&str, &str, &str)> = ch
            .fields_changed
            .iter()
            .filter_map(|fc| {
                fc.name_changed
                    .as_ref()
                    .map(|(f, t)| (fc.id.as_str(), f.as_str(), t.as_str()))
            })
            .collect();
        let mut claimed_cols: HashSet<&str> = ch
            .fields_added
            .iter()
            .filter_map(|fid| field_by_id(new_e, fid))
            .map(|f| f.name.as_str())
            .collect();
        claimed_cols.extend(col_renames.iter().map(|(_, _, t)| *t));
        for fid in &ch.fields_removed {
            let f = field_by_id(old_e, fid).expect("removed field exists in old");
            if claimed_cols.contains(f.name.as_str()) {
                hoisted_columns
                    .entry(ch.id.as_str())
                    .or_default()
                    .insert(fid.as_str());
                hoisted_col_fields
                    .entry(ch.id.as_str())
                    .or_default()
                    .push(f);
            }
        }
        if !col_renames.is_empty() {
            col_renames_by.insert(ch.id.as_str(), col_renames);
        }
    }

    // ---- Preamble 2: partition changed entities' dropped constraints /
    // indexes into hoisted (emitted here) and plain (kept in the destructive
    // tail). Hoisted are: name-freeing drops — a rename's own target may be a
    // name freed only here, so they must precede the renames (DROP INDEX
    // references no table name; a constraint drop references its table by the
    // OLD, pre-rename name, which is what the table is still called at this
    // point) — plus ALL drops of an entity that hoists a column drop: the
    // hoisted DROP COLUMN implicitly drops constraints/indexes involving the
    // column, and a tail drop would then fail on the missing object.
    let mut retained_constraints: HashMap<&str, Vec<&Constraint>> = HashMap::new();
    let mut retained_indexes: HashMap<&str, Vec<&Index>> = HashMap::new();
    for ch in &d.entities_changed {
        let old_e = old_by_id[ch.id.as_str()];
        let new_e = new_by_id[ch.id.as_str()];
        let entity_hoists_a_column = hoisted_columns.contains_key(ch.id.as_str());
        // Constraint names are also table-scoped (pg_constraint): a same-table
        // redefinition (same name, changed definition) diffs as drop + add and
        // must drop first even when no backing index is involved (CHECK).
        let readded: HashSet<&str> = new_e
            .constraints
            .iter()
            .filter(|c| !old_e.constraints.iter().any(|o| o == *c))
            .map(|c| c.name())
            .collect();
        for c in dropped_constraints(old_e, new_e) {
            let hoist = entity_hoists_a_column
                || readded.contains(c.name())
                || unique_constraint_name(c).is_some_and(|n| claimed.contains(n));
            if hoist {
                plan.push(drop_constraint_op(old_e, c));
            } else {
                retained_constraints
                    .entry(ch.id.as_str())
                    .or_default()
                    .push(c);
            }
        }
        for i in dropped_indexes(old_e, new_e) {
            if entity_hoists_a_column || claimed.contains(i.name.as_str()) {
                plan.push(drop_index_op(new_e, i));
            } else {
                retained_indexes.entry(ch.id.as_str()).or_default().push(i);
            }
        }
    }

    // ---- Preamble 3: ALL table renames, dependency-ordered — a rename whose
    // target name is freed by another pending rename waits for it. A cycle
    // (a swap) has no rename-only order and is rejected. Each rename takes
    // its implicit pkey along (index names do not follow a table rename;
    // keeping `<table>_pkey` canonical prevents the recreated table's pkey
    // silently drifting to a suffixed name and a LATER migration's
    // aside-rename grabbing a live table's index).
    let mut pending = renames;
    while !pending.is_empty() {
        let held: HashSet<&str> = pending.iter().map(|(o, _)| o.name.as_str()).collect();
        let (ready, blocked): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|(_, n)| !held.contains(n.name.as_str()));
        if ready.is_empty() {
            return Err(CompileError::TableRenameCycle {
                names: blocked.iter().map(|(o, _)| o.name.clone()).collect(),
            });
        }
        for (o, n) in ready {
            plan.push(rename_table_op(o, n));
            plan.push(rename_pkey_op(o, n));
        }
        pending = blocked;
    }

    // ---- Preamble 4: per-entity column-namespace freeing (analysis above).
    // Same free-before-claim rule one level down (columns are table-scoped):
    // the hoisted column drops run now, and ALL column renames run here,
    // dependency-ordered within their table — a same-name redefinition would
    // otherwise ADD before DROP (42701), and a rename into a freed name would
    // run after its claimer. These reference the table by its post-rename
    // name, hence after the table renames above.
    for ch in &d.entities_changed {
        let new_e = new_by_id[ch.id.as_str()];
        for f in hoisted_col_fields.get(ch.id.as_str()).into_iter().flatten() {
            plan.push(drop_column_op(new_e, f));
        }
        let mut pending = col_renames_by.remove(ch.id.as_str()).unwrap_or_default();
        while !pending.is_empty() {
            let held: HashSet<&str> = pending.iter().map(|(_, from, _)| *from).collect();
            let (ready, blocked): (Vec<_>, Vec<_>) = pending
                .into_iter()
                .partition(|(_, _, to)| !held.contains(to));
            if ready.is_empty() {
                return Err(CompileError::ColumnRenameCycle {
                    entity: new_e.id.to_string(),
                    names: blocked
                        .iter()
                        .map(|(_, from, _)| from.to_string())
                        .collect(),
                });
            }
            for (fid, from, to) in ready {
                plan.push(rename_column_op(new_e, fid, from, to));
            }
            pending = blocked;
        }
    }

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

    // 3) Column alters (retype / nullability / default — renames ran in the
    //    preamble).
    for ch in &d.entities_changed {
        let new_e = new_by_id[ch.id.as_str()];
        emit_column_alters(&mut plan, new_e, ch, &names);
    }

    // 4) Destructive removals — drop constraints/indexes, then columns, then
    //    (last) tables, so dependencies unwind cleanly.
    let no_hoisted: HashSet<&str> = HashSet::new();
    for ch in &d.entities_changed {
        let old_e = old_by_id[ch.id.as_str()];
        let new_e = new_by_id[ch.id.as_str()];
        emit_destructive_changes(
            &mut plan,
            old_e,
            new_e,
            ch,
            retained_constraints
                .get(ch.id.as_str())
                .map_or(&[][..], |v| v.as_slice()),
            retained_indexes
                .get(ch.id.as_str())
                .map_or(&[][..], |v| v.as_slice()),
            hoisted_columns.get(ch.id.as_str()).unwrap_or(&no_hoisted),
        );
    }
    // 4b) Dropped tables, DEPENDENTS-FIRST. `d.entities_removed` arrives in
    //     entity-id lexical order (a BTreeMap in the diff), which is FK-blind:
    //     a removed child B whose `Reference` field targets a removed parent A
    //     holds an FK B -> A, so dropping A before B fails 2BP01 (the
    //     constraint on B depends on A) and the one-txn apply rolls back.
    //     Topologically order the removed set on its inbound `Reference` edges
    //     (Kahn rounds, mirroring the table-rename ordering in preamble 3) and
    //     emit each table before the tables it references. Only edges WITHIN
    //     the removed set matter: a `Reference` to a RETAINED table keeps its
    //     FK on the retained side, and the new catalog cannot retain a
    //     `Reference` to a removed table (validation rejects the dangling
    //     target). A self-edge (a tree's parent pointer) drops with the table,
    //     so it is ignored. A mutual-FK cycle among dropped tables has no
    //     linearization and is rejected (`DropCycle`), as the rename cycles are.
    let removed_set: HashSet<&str> = d.entities_removed.iter().map(|e| e.as_str()).collect();
    let mut pending: Vec<&str> = d.entities_removed.iter().map(|e| e.as_str()).collect();
    let mut drop_order: Vec<&str> = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        // A table is still held if any pending table references it — that
        // referencer (the child) must drop first, so the parent is not ready.
        let mut referenced: HashSet<&str> = HashSet::new();
        for id in &pending {
            for f in &old_by_id[*id].fields {
                if let FieldType::Reference { entity: target } = &f.field_type {
                    let t = target.as_str();
                    if t != *id && removed_set.contains(t) {
                        referenced.insert(t);
                    }
                }
            }
        }
        let (ready, blocked): (Vec<&str>, Vec<&str>) = pending
            .into_iter()
            .partition(|id| !referenced.contains(*id));
        if ready.is_empty() {
            return Err(CompileError::DropCycle {
                entities: blocked.iter().map(|s| s.to_string()).collect(),
            });
        }
        drop_order.extend(ready);
        pending = blocked;
    }
    for id in &drop_order {
        let e = old_by_id[*id];
        let (drop_ident, note) = match temp_named.get(*id) {
            Some(tmp) => (
                tmp.as_str(),
                "drops the table and all its data (renamed aside at the top of this plan to free its reused name)",
            ),
            None => (e.name.as_str(), "drops the table and all its data"),
        };
        plan.push(Operation {
            summary: format!("drop table {}", e.name),
            sql: format!("DROP TABLE {}", q(drop_ident)),
            safety: Safety::Destructive,
            entity: e.id.to_string(),
            field: None,
            note: Some(note.into()),
        });
    }

    Ok(plan)
}

/// Unique constraints create a backing index carrying the constraint's name,
/// so they live in the schema-wide relation namespace (with tables and
/// indexes); CHECK constraints are table-scoped and do not.
fn unique_constraint_name(c: &Constraint) -> Option<&str> {
    match c {
        Constraint::Unique { name, .. } => Some(name),
        Constraint::Check { .. } => None,
    }
}

/// Constraints present in `old_e` but not in `new_e` (identity = whole value).
fn dropped_constraints<'a>(old_e: &'a Entity, new_e: &Entity) -> Vec<&'a Constraint> {
    old_e
        .constraints
        .iter()
        .filter(|c| !new_e.constraints.iter().any(|n| n == *c))
        .collect()
}

/// Indexes present in `old_e` but not in `new_e` (identity = whole value).
fn dropped_indexes<'a>(old_e: &'a Entity, new_e: &Entity) -> Vec<&'a Index> {
    old_e
        .indexes
        .iter()
        .filter(|i| !new_e.indexes.iter().any(|n| n == *i))
        .collect()
}

/// A changed entity's table rename — hoisted into the plan preamble (see
/// [`migrate_plan`]): the old name is freed for reuse before any add, and
/// every later `ALTER TABLE` on this entity validly references the new name.
fn rename_table_op(old_e: &Entity, new_e: &Entity) -> Operation {
    Operation {
        summary: format!("rename table {} -> {}", old_e.name, new_e.name),
        sql: format!(
            "ALTER TABLE {} RENAME TO {}",
            q(&old_e.name),
            q(&new_e.name)
        ),
        safety: Safety::Destructive,
        entity: new_e.id.to_string(),
        field: None,
        note: Some("breaks generated API / flows referencing the old name".into()),
    }
}

/// Rename a dropped table aside so its name can be reclaimed by an add in the
/// same plan. The actual `DROP TABLE` stays last — foreign keys follow a
/// rename, so the destructive tail's FK unwind order is untouched.
fn temp_rename_table_op(e: &Entity, tmp: &str) -> Operation {
    Operation {
        summary: format!("rename table {} aside as {tmp} (name reused)", e.name),
        sql: format!("ALTER TABLE {} RENAME TO {}", q(&e.name), q(tmp)),
        safety: Safety::Destructive,
        entity: e.id.to_string(),
        field: None,
        note: Some(
            "frees the name for reuse; the renamed-aside table is dropped at the end of this plan"
                .into(),
        ),
    }
}

/// The implicit `<table>_pkey` follows its table's hoisted rename. Index names
/// do not follow a table rename on their own; left stale, the recreated
/// same-named table's pkey silently drifts to a suffixed name (Postgres
/// auto-avoids implicit-name collisions rather than erroring) and a LATER
/// migration's aside-rename of `<table>_pkey` would grab a LIVE table's
/// index. `IF EXISTS` because a pre-drifted pkey keeps its earlier name; a
/// colliding target fails loudly inside the one-transaction apply.
fn rename_pkey_op(old_e: &Entity, new_e: &Entity) -> Operation {
    Operation {
        summary: format!(
            "rename index {}_pkey -> {}_pkey (follows the table rename)",
            old_e.name, new_e.name
        ),
        sql: format!(
            "ALTER INDEX IF EXISTS {} RENAME TO {}",
            q(&format!("{}_pkey", old_e.name)),
            q(&format!("{}_pkey", new_e.name)),
        ),
        safety: Safety::Destructive,
        entity: new_e.id.to_string(),
        field: None,
        note: Some("keeps the implicit primary-key index name canonical".into()),
    }
}

/// Rename a dropped table's index (or unique-constraint backing index) aside:
/// index names do NOT follow a table rename and share the table namespace, so
/// a recreated same-named table's indexes — including its implicit
/// `<table>_pkey` — would otherwise collide or silently drift. `IF EXISTS`
/// because an index created under an earlier table name keeps that name; a
/// skipped rename then fails loudly at the claiming CREATE.
fn temp_rename_index_op(e: &Entity, name: &str) -> Operation {
    Operation {
        summary: format!("rename index {name} aside (dropped with table {})", e.name),
        sql: format!(
            "ALTER INDEX IF EXISTS {} RENAME TO {}",
            q(name),
            q(&format!("{TEMP_DROP_PREFIX}{name}"))
        ),
        safety: Safety::Destructive,
        entity: e.id.to_string(),
        field: None,
        note: None,
    }
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
            entity: new_e.id.to_string(),
            field: Some(f.id.to_string()),
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

/// A column rename — hoisted into the plan preamble (see [`migrate_plan`]):
/// the old column name is freed before any add claims it, and renames within
/// a table are dependency-ordered there.
fn rename_column_op(new_e: &Entity, field_id: &str, from: &str, to: &str) -> Operation {
    Operation {
        summary: format!("rename column {}.{from} -> {to}", new_e.name),
        sql: format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {}",
            q(&new_e.name),
            q(from),
            q(to)
        ),
        safety: Safety::Destructive,
        entity: new_e.id.to_string(),
        field: Some(field_id.to_string()),
        note: Some("breaks generated API / flows referencing the old name".into()),
    }
}

/// Drop a column (and its data).
fn drop_column_op(new_e: &Entity, f: &wamn_catalog::Field) -> Operation {
    Operation {
        summary: format!("drop column {}.{}", new_e.name, f.name),
        sql: format!("ALTER TABLE {} DROP COLUMN {}", q(&new_e.name), q(&f.name)),
        safety: Safety::Destructive,
        entity: new_e.id.to_string(),
        field: Some(f.id.to_string()),
        note: Some("drops the column and its data".into()),
    }
}

/// Column retypes, nullability, and default changes (renames run in the plan
/// preamble — see [`migrate_plan`]).
fn emit_column_alters(
    plan: &mut MigrationPlan,
    new_e: &Entity,
    ch: &wamn_catalog::EntityChange,
    names: &HashMap<&str, &str>,
) {
    for fc in &ch.fields_changed {
        let f = field_by_id(new_e, &fc.id).expect("changed field exists in new");
        if let Some((old_ty, _)) = &fc.type_changed {
            // A reference field's FK is synthesized (fk_op), never a catalog
            // constraint, so a retype does not carry it. Drop a stale FK when
            // the type LEAVES Reference (before the retype: the FK depends on
            // the column and the referenced table, and a survivor blocks both
            // an incompatible retype and a later DROP of a removed target,
            // 2BP01); add the FK when the type ENTERS Reference (after the
            // retype: the column is uuid by then). A Reference re-pointed at a
            // new target does both — drop the old-named FK, add the new one.
            if matches!(old_ty, FieldType::Reference { .. }) {
                let old_name = fc
                    .name_changed
                    .as_ref()
                    .map_or(f.name.as_str(), |(o, _)| o.as_str());
                plan.push(drop_reference_fk_op(new_e, old_name));
            }
            let ty = sql_type(&f.field_type);
            plan.push(Operation {
                summary: format!("retype column {}.{}", new_e.name, f.name),
                sql: format!(
                    "ALTER TABLE {tbl} ALTER COLUMN {col} TYPE {ty} USING {col}::{ty}",
                    tbl = q(&new_e.name),
                    col = q(&f.name),
                ),
                safety: Safety::Destructive,
                entity: new_e.id.to_string(),
                field: Some(fc.id.to_string()),
                note: Some("cast may fail or truncate existing values".into()),
            });
            if let FieldType::Reference { entity: target } = &f.field_type {
                let target_table = names.get(target.as_str()).copied().unwrap_or(target);
                plan.push(fk_op(new_e, f, target_table));
            }
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
                entity: new_e.id.to_string(),
                field: Some(fc.id.to_string()),
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
                entity: new_e.id.to_string(),
                field: Some(fc.id.to_string()),
                note: None,
            });
        }
    }
}

/// Drop the synthesized FK of a reference field. Used when a retype LEAVES
/// `Reference` (Reference -> non-Reference, or a Reference re-pointed at a new
/// target): the FK is not carried by `ALTER COLUMN TYPE`, so a stale one would
/// block an incompatible retype and any later `DROP TABLE` of the (possibly
/// removed) referenced table (2BP01). `field_name` is the column's name as the
/// FK was created under — the pre-rename name if this bump also renames it,
/// since a constraint name does not follow a column rename.
fn drop_reference_fk_op(new_e: &Entity, field_name: &str) -> Operation {
    Operation {
        summary: format!("drop foreign key {}.{field_name}", new_e.name),
        sql: format!(
            "ALTER TABLE {} DROP CONSTRAINT {}",
            q(&new_e.name),
            q(&fk_constraint_name(&new_e.name, field_name))
        ),
        safety: Safety::Destructive,
        entity: new_e.id.to_string(),
        field: None,
        note: Some("removes the foreign-key integrity guarantee".into()),
    }
}

/// Drop a table constraint (removes a guarantee — needs review).
fn drop_constraint_op(new_e: &Entity, c: &Constraint) -> Operation {
    Operation {
        summary: format!("drop constraint {} on {}", c.name(), new_e.name),
        sql: format!(
            "ALTER TABLE {} DROP CONSTRAINT {}",
            q(&new_e.name),
            q(c.name())
        ),
        safety: Safety::Destructive,
        entity: new_e.id.to_string(),
        field: None,
        note: Some("removes a data-integrity guarantee".into()),
    }
}

/// Drop an index: a unique index drop relaxes a guarantee (destructive); a
/// plain index drop is data-safe (additive).
fn drop_index_op(new_e: &Entity, idx: &Index) -> Operation {
    Operation {
        summary: format!("drop index {}", idx.name),
        sql: format!("DROP INDEX {}", q(&idx.name)),
        safety: if idx.unique {
            Safety::Destructive
        } else {
            Safety::Additive
        },
        entity: new_e.id.to_string(),
        field: None,
        note: idx.unique.then(|| "removes a uniqueness guarantee".into()),
    }
}

/// Dropped columns plus the retained (non-name-freeing) constraint and index
/// drops. Name-freeing drops, hoisted column drops, and the table/column
/// renames run in the plan preamble ([`migrate_plan`]).
fn emit_destructive_changes(
    plan: &mut MigrationPlan,
    old_e: &Entity,
    new_e: &Entity,
    ch: &wamn_catalog::EntityChange,
    retained_constraints: &[&Constraint],
    retained_indexes: &[&Index],
    hoisted_columns: &HashSet<&str>,
) {
    for c in retained_constraints {
        plan.push(drop_constraint_op(new_e, c));
    }
    for idx in retained_indexes {
        plan.push(drop_index_op(new_e, idx));
    }
    // Dropped columns (minus those hoisted to free a reused name).
    for fid in &ch.fields_removed {
        if hoisted_columns.contains(fid.as_str()) {
            continue;
        }
        let f = field_by_id(old_e, fid).expect("removed field exists in old");
        plan.push(drop_column_op(new_e, f));
    }
}

#[cfg(test)]
mod tests {
    use super::migrate_plan;
    use crate::CompileError;
    use wamn_catalog::{Catalog, Entity, Field, FieldType, Index};

    fn text_field(name: &str) -> Field {
        Field {
            id: name.into(),
            name: name.into(),
            field_type: FieldType::Text { max_len: None },
            nullable: false,
            default: None,
            sensitive: false,
            is_system: false,
            label: None,
            description: None,
        }
    }

    fn entity(id: &str, name: &str, fields: Vec<Field>) -> Entity {
        Entity {
            id: id.into(),
            name: name.into(),
            is_system: false,
            label: None,
            description: None,
            fields,
            indexes: vec![],
            constraints: vec![],
        }
    }

    fn mini(version: u32, entities: Vec<Entity>) -> Catalog {
        Catalog {
            schema_version: "0.1".into(),
            catalog_id: "c".into(),
            version,
            name: None,
            entities,
            relations: vec![],
        }
    }

    /// Defense-in-depth: `migrate_plan` is the internal, UNVALIDATED entry
    /// (`Migration::migrate` runs `check()` — hence `validate()` — first, so
    /// wamn-66x's reserved-prefix rule now rejects any `wamn_`-prefixed name
    /// before this point; see the integration test
    /// `reserved_prefix_name_is_rejected_before_the_aside_collision`). This pins
    /// the k56 aside-name collision guard for a caller that reaches `migrate_plan`
    /// without validating — the aside the plan needs is itself a real relation.
    #[test]
    fn migrate_plan_rejects_aside_name_collision() {
        // A dropped-and-reclaimed table whose aside `wamn_mig_drop_audit` is a
        // real table.
        let v1 = mini(
            1,
            vec![
                entity("x", "audit", vec![text_field("v")]),
                entity("t", "wamn_mig_drop_audit", vec![text_field("v")]),
            ],
        );
        let v2 = mini(
            2,
            vec![
                entity("e", "audit", vec![text_field("v")]),
                entity("t", "wamn_mig_drop_audit", vec![text_field("v")]),
            ],
        );
        match migrate_plan(&v1, &v2) {
            Err(CompileError::TempNameCollision { name }) => {
                assert_eq!(name, "wamn_mig_drop_audit")
            }
            other => panic!("expected TempNameCollision, got {other:?}"),
        }

        // The check spans the whole relation namespace: an INDEX aside-target
        // colliding with a real index is rejected too (indexes share pg_class
        // with tables).
        let mut x = entity("x", "audit", vec![text_field("v")]);
        x.indexes.push(Index {
            name: "ix".into(),
            fields: vec!["v".into()],
            unique: false,
        });
        let mut t = entity("t", "keeper", vec![text_field("v")]);
        t.indexes.push(Index {
            name: "wamn_mig_drop_ix".into(),
            fields: vec!["v".into()],
            unique: false,
        });
        let v1 = mini(1, vec![x, t.clone()]);
        let v2 = mini(2, vec![entity("e", "audit", vec![text_field("v")]), t]);
        match migrate_plan(&v1, &v2) {
            Err(CompileError::TempNameCollision { name }) => {
                assert_eq!(name, "wamn_mig_drop_ix")
            }
            other => panic!("expected TempNameCollision for the index aside, got {other:?}"),
        }
    }
}
