//! PostgreSQL 18 `pg_catalog` reader for the normalized schema IR.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use sqlx::postgres::PgRow as Row;
use sqlx::{
    AssertSqlSafe, Executor as _, PgPool as Client, Row as _, SqlSafeStr as _, Statement as _,
};
use wamn_record_history::is_history_table_name;

use crate::ir::{
    CatalogIr, Column, ColumnGeneration, Constraint, Exclusion, ExclusionAccessMethod,
    ExclusionElement, ExclusionKey, ForeignKeyAction, IdentityMode, Index, IndexColumn,
    IndexDirection, IrError, IrErrorType, Table, View, ViewBaseTable, ViewColumn, ViewColumnOrigin,
    postgres_default, postgres_type,
};
use wamn_catalog::schema::ForeignKeyColumn;

/// Stable class of PostgreSQL catalog refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostgresIntrospectionErrorType {
    Database,
    MissingSchema,
    UnsupportedRelation,
    UnsupportedColumnType,
    UnsupportedColumnDefault,
    UnsupportedGeneratedColumn,
    UnsupportedIdentity,
    UnsupportedConstraint,
    UnsupportedIndex,
}

impl PostgresIntrospectionErrorType {
    const fn code(self) -> &'static str {
        match self {
            Self::Database => "database",
            Self::MissingSchema => "missing-schema",
            Self::UnsupportedRelation => "unsupported-relation",
            Self::UnsupportedColumnType => "unsupported-column-type",
            Self::UnsupportedColumnDefault => "unsupported-column-default",
            Self::UnsupportedGeneratedColumn => "unsupported-generated-column",
            Self::UnsupportedIdentity => "unsupported-identity",
            Self::UnsupportedConstraint => "unsupported-constraint",
            Self::UnsupportedIndex => "unsupported-index",
        }
    }
}

/// Contextual refusal at the PostgreSQL catalog boundary.
#[derive(Debug)]
pub struct PostgresIntrospectionError {
    type_: PostgresIntrospectionErrorType,
    schema: Option<Box<str>>,
    object: Option<Box<str>>,
    detail: Box<str>,
    source: Option<sqlx::Error>,
}

impl PostgresIntrospectionError {
    /// Stable refusal class.
    pub const fn error_type(&self) -> PostgresIntrospectionErrorType {
        self.type_
    }

    /// Configured schema involved in the refusal, if any.
    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    /// Unqualified catalog object involved in the refusal, if any.
    pub fn object(&self) -> Option<&str> {
        self.object.as_deref()
    }

    /// Stable contextual detail supplied by the reader.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for PostgresIntrospectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "PostgreSQL introspection refused ({})",
            self.type_.code()
        )?;
        if let Some(schema) = &self.schema {
            write!(formatter, " in schema `{schema}`")?;
        }
        if let Some(object) = &self.object {
            write!(formatter, " for `{object}`")?;
        }
        write!(formatter, ": {}", self.detail)
    }
}

impl std::error::Error for PostgresIntrospectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|source| source as _)
    }
}

#[derive(Debug)]
struct TableParts {
    columns: Vec<Column>,
    constraints: Vec<Constraint>,
    indexes: Vec<Index>,
    exclusions: Vec<Exclusion>,
}

#[derive(Debug)]
struct RelationRow {
    schema: String,
    name: String,
}

#[derive(Debug)]
struct ColumnRow {
    schema: String,
    table: String,
    number: i16,
    name: String,
    type_name: String,
    nullable: bool,
    default_expression: Option<String>,
    identity: String,
    generated: String,
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the row mirrors independent PostgreSQL pg_constraint and pg_index flags"
)]
struct ConstraintRow {
    schema: String,
    table: String,
    name: String,
    type_: String,
    columns: Vec<i16>,
    referenced_schema: Option<String>,
    referenced_table: Option<String>,
    referenced_columns: Vec<i16>,
    on_update: String,
    on_delete: String,
    match_type: String,
    expression: Option<String>,
    deferrable: bool,
    initially_deferred: bool,
    enforced: bool,
    validated: bool,
    local: bool,
    inherited_count: i16,
    no_inherit: bool,
    period: bool,
    parent: bool,
    delete_column_subset: bool,
    supporting_index_method: Option<String>,
    supporting_index_keys: Option<i16>,
    supporting_index_attributes: Option<i16>,
    supporting_index_nulls_not_distinct: Option<bool>,
    supporting_index_default_operator_classes: bool,
    supporting_index_default_collations: bool,
    supporting_index_expression: bool,
    supporting_index_predicate: bool,
    exclusion_operators: Vec<String>,
    exclusion_key_definitions: Vec<String>,
    exclusion_dependency_columns: Vec<i16>,
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the row mirrors independent PostgreSQL pg_index flags"
)]
struct IndexRow {
    schema: String,
    table: String,
    name: String,
    access_method: String,
    unique: bool,
    nulls_not_distinct: bool,
    exclusion: bool,
    immediate: bool,
    valid: bool,
    ready: bool,
    live: bool,
    attributes: i16,
    keys: i16,
    column_numbers: Vec<i16>,
    options: Vec<i16>,
    default_operator_classes: Vec<bool>,
    default_collations: Vec<bool>,
    expression: bool,
    predicate: bool,
}

type TableKey = (String, String);
type AttributeKey = (String, String, i16);

/// Whether the reader skips a relation and every catalog row that belongs to it.
///
/// The caller names host-owned relations. A record history table is a platform
/// fixture, so its `_history` name alone skips it.
fn relation_is_excluded(excluded_relations: &[(&str, &str)], schema: &str, table: &str) -> bool {
    is_history_table_name(table)
        || excluded_relations
            .iter()
            .any(|(excluded_schema, excluded_table)| {
                *excluded_schema == schema && *excluded_table == table
            })
}

/// Application role shared with provisioning.
pub const DATA_ACCESS_ROLE: &str = "wamn_app";

const SCHEMAS_SQL: &str = "SELECT configured.schema_name, namespace.oid IS NOT NULL AS present FROM unnest($1::text[]) AS configured(schema_name) LEFT JOIN pg_catalog.pg_namespace namespace ON namespace.nspname = configured.schema_name";

const RELATIONS_SQL: &str = "SELECT namespace.nspname::text AS schema_name, relation.relname::text AS relation_name FROM pg_catalog.pg_class AS relation JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace WHERE namespace.nspname = ANY($1::text[]) AND relation.relkind = 'r' ORDER BY 1, 2";

const VIEW_COLUMNS_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       relation.relname::text AS view_name,
       attribute.attname::text AS column_name,
       catalog_type.typname::text AS type_name
  FROM pg_catalog.pg_views AS view_catalog
  JOIN pg_catalog.pg_namespace AS namespace ON namespace.nspname = view_catalog.schemaname
  JOIN pg_catalog.pg_class AS relation
    ON relation.relnamespace = namespace.oid AND relation.relname = view_catalog.viewname
  LEFT JOIN pg_catalog.pg_attribute AS attribute
    ON attribute.attrelid = relation.oid AND attribute.attnum > 0 AND NOT attribute.attisdropped
  LEFT JOIN pg_catalog.pg_type AS catalog_type ON catalog_type.oid = attribute.atttypid
 WHERE namespace.nspname = ANY($1::text[])
 ORDER BY namespace.nspname, relation.relname, attribute.attnum
";

const VIEW_BASE_TABLES_SQL: &str = r"
WITH RECURSIVE dependencies(view_oid, relation_oid) AS (
    SELECT relation.oid, relation.oid
      FROM pg_catalog.pg_views AS view_catalog
      JOIN pg_catalog.pg_namespace AS namespace ON namespace.nspname = view_catalog.schemaname
      JOIN pg_catalog.pg_class AS relation
        ON relation.relnamespace = namespace.oid AND relation.relname = view_catalog.viewname
     WHERE namespace.nspname = ANY($1::text[])
    UNION
    SELECT dependencies.view_oid, dependency.refobjid
      FROM dependencies
      JOIN pg_catalog.pg_rewrite AS rewrite
        ON rewrite.ev_class = dependencies.relation_oid AND rewrite.rulename = '_RETURN'
      JOIN pg_catalog.pg_depend AS dependency
        ON dependency.classid = 'pg_catalog.pg_rewrite'::regclass
       AND dependency.objid = rewrite.oid
       AND dependency.refclassid = 'pg_catalog.pg_class'::regclass
       AND dependency.refobjid <> rewrite.ev_class
)
SELECT view_namespace.nspname::text AS view_schema,
       view_relation.relname::text AS view_name,
       base_namespace.nspname::text AS base_schema,
       base_relation.relname::text AS base_name
  FROM dependencies
  JOIN pg_catalog.pg_class AS view_relation ON view_relation.oid = dependencies.view_oid
  JOIN pg_catalog.pg_namespace AS view_namespace ON view_namespace.oid = view_relation.relnamespace
  JOIN pg_catalog.pg_class AS base_relation ON base_relation.oid = dependencies.relation_oid
  JOIN pg_catalog.pg_namespace AS base_namespace ON base_namespace.oid = base_relation.relnamespace
 WHERE dependencies.relation_oid <> dependencies.view_oid
   AND NOT EXISTS (
       SELECT FROM pg_catalog.pg_rewrite AS rewrite
        WHERE rewrite.ev_class = base_relation.oid AND rewrite.rulename = '_RETURN'
   )
 ORDER BY view_namespace.nspname, view_relation.relname, base_namespace.nspname, base_relation.relname
";

const COLUMNS_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       relation.relname::text AS table_name,
       attribute.attnum AS column_number,
       attribute.attname::text AS column_name,
       catalog_type.typname::text AS type_name,
       NOT attribute.attnotnull AS nullable,
       pg_catalog.pg_get_expr(attribute_default.adbin, attribute_default.adrelid, false)
           AS default_expression,
       attribute.attidentity::text AS identity_kind,
       attribute.attgenerated::text AS generated_kind
  FROM pg_catalog.pg_attribute AS attribute
  JOIN pg_catalog.pg_class AS relation
    ON relation.oid = attribute.attrelid
  JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = relation.relnamespace
  JOIN pg_catalog.pg_type AS catalog_type
    ON catalog_type.oid = attribute.atttypid
  LEFT JOIN pg_catalog.pg_attrdef AS attribute_default
    ON attribute_default.adrelid = attribute.attrelid
   AND attribute_default.adnum = attribute.attnum
 WHERE namespace.nspname = ANY($1::text[])
   AND relation.relkind = 'r'
   AND attribute.attnum > 0
   AND NOT attribute.attisdropped
 ORDER BY namespace.nspname, relation.relname, attribute.attnum
";

const CONSTRAINTS_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       relation.relname::text AS table_name,
       constraint_row.conname::text AS constraint_name,
       constraint_row.contype::text AS constraint_kind,
       COALESCE(constraint_row.conkey, ARRAY[]::smallint[]) AS column_numbers,
       referenced_namespace.nspname::text AS referenced_schema,
       referenced_relation.relname::text AS referenced_table,
       COALESCE(constraint_row.confkey, ARRAY[]::smallint[]) AS referenced_column_numbers,
       constraint_row.confupdtype::text AS on_update,
       constraint_row.confdeltype::text AS on_delete,
       constraint_row.confmatchtype::text AS match_type,
       pg_catalog.pg_get_expr(constraint_row.conbin, constraint_row.conrelid, false)
           AS check_expression,
       constraint_row.condeferrable AS deferrable,
       constraint_row.condeferred AS initially_deferred,
       constraint_row.conenforced AS enforced,
       constraint_row.convalidated AS validated,
       constraint_row.conislocal AS is_local,
       constraint_row.coninhcount AS inherited_count,
       constraint_row.connoinherit AS no_inherit,
       constraint_row.conperiod AS is_period,
       constraint_row.conparentid <> 0 AS has_parent,
       constraint_row.confdelsetcols IS NOT NULL AS has_delete_column_subset,
       supporting_method.amname::text AS supporting_index_method,
       supporting_index.indnkeyatts AS supporting_index_keys,
       supporting_index.indnatts AS supporting_index_attributes,
       supporting_index.indnullsnotdistinct AS supporting_index_nulls_not_distinct,
       NOT EXISTS (
           SELECT 1
             FROM unnest(supporting_index.indclass::oid[]) WITH ORDINALITY
                  AS key_operator_class(operator_class_oid, position)
             JOIN pg_catalog.pg_opclass AS operator_class
               ON operator_class.oid = key_operator_class.operator_class_oid
            WHERE key_operator_class.position <= supporting_index.indnkeyatts
              AND NOT operator_class.opcdefault
       ) AS supporting_index_has_default_operator_classes,
       NOT EXISTS (
           SELECT 1
             FROM unnest(
                      supporting_index.indkey::smallint[],
                      supporting_index.indcollation::oid[]
                  ) WITH ORDINALITY
                  AS key_collation(column_number, collation_oid, position)
             LEFT JOIN pg_catalog.pg_attribute AS attribute
               ON attribute.attrelid = supporting_index.indrelid
              AND attribute.attnum = key_collation.column_number
            WHERE key_collation.position <= supporting_index.indnkeyatts
              AND key_collation.column_number <> 0
              AND NOT COALESCE(key_collation.collation_oid = attribute.attcollation, false)
       ) AS supporting_index_has_default_collations,
       supporting_index.indexprs IS NOT NULL AS supporting_index_has_expression,
       supporting_index.indpred IS NOT NULL AS supporting_index_has_predicate,
       ARRAY(
           SELECT exclusion_operator.oprname::text
             FROM unnest(constraint_row.conexclop) WITH ORDINALITY
                  AS key_operator(operator_oid, position)
             JOIN pg_catalog.pg_operator AS exclusion_operator
               ON exclusion_operator.oid = key_operator.operator_oid
            ORDER BY key_operator.position
       ) AS exclusion_operators,
       ARRAY(
           SELECT pg_catalog.pg_get_indexdef(
                      supporting_index.indexrelid, key_definition.position::int, true)
             FROM unnest(supporting_index.indkey::smallint[]) WITH ORDINALITY
                  AS key_definition(column_number, position)
            WHERE constraint_row.contype = 'x'
              AND key_definition.position <= supporting_index.indnkeyatts
            ORDER BY key_definition.position
       ) AS exclusion_key_definitions,
       ARRAY(
           SELECT DISTINCT dependency.refobjsubid::smallint
             FROM pg_catalog.pg_depend AS dependency
            WHERE constraint_row.contype = 'x'
              AND dependency.deptype = 'a'
              AND dependency.refclassid = 'pg_catalog.pg_class'::regclass
              AND dependency.refobjid = constraint_row.conrelid
              AND dependency.refobjsubid > 0
              AND ((dependency.classid = 'pg_catalog.pg_constraint'::regclass
                    AND dependency.objid = constraint_row.oid)
                OR (dependency.classid = 'pg_catalog.pg_class'::regclass
                    AND dependency.objid = constraint_row.conindid))
            ORDER BY 1
       ) AS exclusion_dependency_columns
  FROM pg_catalog.pg_constraint AS constraint_row
  JOIN pg_catalog.pg_class AS relation
    ON relation.oid = constraint_row.conrelid
  JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = relation.relnamespace
  LEFT JOIN pg_catalog.pg_class AS referenced_relation
    ON referenced_relation.oid = constraint_row.confrelid
  LEFT JOIN pg_catalog.pg_namespace AS referenced_namespace
    ON referenced_namespace.oid = referenced_relation.relnamespace
  LEFT JOIN pg_catalog.pg_index AS supporting_index
    ON supporting_index.indexrelid = constraint_row.conindid
  LEFT JOIN pg_catalog.pg_class AS supporting_index_relation
    ON supporting_index_relation.oid = supporting_index.indexrelid
  LEFT JOIN pg_catalog.pg_am AS supporting_method
    ON supporting_method.oid = supporting_index_relation.relam
 WHERE namespace.nspname = ANY($1::text[])
   AND relation.relkind = 'r'
   -- A constraint trigger is a trigger, which the trigger query judges.
   AND constraint_row.contype <> 't'
 ORDER BY namespace.nspname, relation.relname, constraint_row.conname
";

const INDEXES_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       table_relation.relname::text AS table_name,
       index_relation.relname::text AS index_name,
       access_method.amname::text AS access_method,
       catalog_index.indisunique AS is_unique,
       catalog_index.indnullsnotdistinct AS nulls_not_distinct,
       catalog_index.indisexclusion AS is_exclusion,
       catalog_index.indimmediate AS is_immediate,
       catalog_index.indisvalid AS is_valid,
       catalog_index.indisready AS is_ready,
       catalog_index.indislive AS is_live,
       catalog_index.indnatts AS attribute_count,
       catalog_index.indnkeyatts AS key_count,
       catalog_index.indkey::smallint[] AS column_numbers,
       catalog_index.indoption::smallint[] AS key_options,
       ARRAY(
           SELECT operator_class.opcdefault
             FROM unnest(catalog_index.indclass::oid[]) WITH ORDINALITY
                  AS key_operator_class(operator_class_oid, position)
             JOIN pg_catalog.pg_opclass AS operator_class
               ON operator_class.oid = key_operator_class.operator_class_oid
            WHERE key_operator_class.position <= catalog_index.indnkeyatts
            ORDER BY key_operator_class.position
       ) AS default_operator_classes,
       ARRAY(
           SELECT COALESCE(key_collation.collation_oid = attribute.attcollation, false)
             FROM unnest(
                      catalog_index.indkey::smallint[],
                      catalog_index.indcollation::oid[]
                  ) WITH ORDINALITY
                  AS key_collation(column_number, collation_oid, position)
             LEFT JOIN pg_catalog.pg_attribute AS attribute
               ON attribute.attrelid = catalog_index.indrelid
              AND attribute.attnum = key_collation.column_number
            WHERE key_collation.position <= catalog_index.indnkeyatts
            ORDER BY key_collation.position
       ) AS default_collations,
       catalog_index.indexprs IS NOT NULL AS has_expression,
       catalog_index.indpred IS NOT NULL AS has_predicate
  FROM pg_catalog.pg_index AS catalog_index
  JOIN pg_catalog.pg_class AS index_relation
    ON index_relation.oid = catalog_index.indexrelid
  JOIN pg_catalog.pg_class AS table_relation
    ON table_relation.oid = catalog_index.indrelid
  JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = table_relation.relnamespace
  JOIN pg_catalog.pg_am AS access_method
    ON access_method.oid = index_relation.relam
 WHERE namespace.nspname = ANY($1::text[])
   AND table_relation.relkind = 'r'
   AND NOT EXISTS (
       SELECT 1
         FROM pg_catalog.pg_constraint AS backing_constraint
        WHERE backing_constraint.conindid = catalog_index.indexrelid
   )
 ORDER BY namespace.nspname, table_relation.relname, index_relation.relname
";

fn refusal(
    type_: PostgresIntrospectionErrorType,
    schema: Option<&str>,
    object: Option<&str>,
    detail: impl Into<Box<str>>,
) -> PostgresIntrospectionError {
    PostgresIntrospectionError {
        type_,
        schema: schema.map(Into::into),
        object: object.map(Into::into),
        detail: detail.into(),
        source: None,
    }
}

fn database_error(context: &'static str, source: sqlx::Error) -> PostgresIntrospectionError {
    PostgresIntrospectionError {
        type_: PostgresIntrospectionErrorType::Database,
        schema: None,
        object: None,
        detail: context.into(),
        source: Some(source),
    }
}

fn ir_error(schema: &str, object: &str, error: &IrError) -> PostgresIntrospectionError {
    let type_ = match error.error_type() {
        IrErrorType::EmptyName => PostgresIntrospectionErrorType::UnsupportedConstraint,
        IrErrorType::UnsupportedType => PostgresIntrospectionErrorType::UnsupportedColumnType,
        IrErrorType::UnsupportedDefault => PostgresIntrospectionErrorType::UnsupportedColumnDefault,
    };
    refusal(type_, Some(schema), Some(object), error.to_string())
}

fn configured_schemas(application_schemas: &[&str]) -> Vec<String> {
    application_schemas
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

async fn validate_schemas(
    client: &Client,
    schemas: &[String],
) -> Result<(), PostgresIntrospectionError> {
    let rows = sqlx::query(AssertSqlSafe((SCHEMAS_SQL).to_owned()).into_sql_str())
        .bind(schemas)
        .fetch_all(client)
        .await
        .map_err(|error| database_error("query configured schemas", error))?;

    for row in rows {
        let schema = row.get::<String, _>("schema_name");
        if !row.get::<bool, _>("present") {
            return Err(refusal(
                PostgresIntrospectionErrorType::MissingSchema,
                Some(&schema),
                None,
                "configured application schema does not exist",
            ));
        }
    }
    Ok(())
}

/// Read the catalog's views without imposing table or synchronization policy.
pub async fn read_views(
    client: &Client,
    application_schemas: &[&str],
) -> Result<Vec<View>, PostgresIntrospectionError> {
    load_views(client, &configured_schemas(application_schemas), &[]).await
}

async fn load_views(
    client: &Client,
    schemas: &[String],
    excluded_relations: &[(&str, &str)],
) -> Result<Vec<View>, PostgresIntrospectionError> {
    let mut columns = BTreeMap::<TableKey, Vec<ViewColumn>>::new();
    for row in sqlx::query(AssertSqlSafe((VIEW_COLUMNS_SQL).to_owned()).into_sql_str())
        .bind(schemas)
        .fetch_all(client)
        .await
        .map_err(|error| database_error("query configured-schema view columns", error))?
    {
        let schema = row.get::<String, _>("schema_name");
        let name = row.get::<String, _>("view_name");
        if relation_is_excluded(excluded_relations, &schema, &name) {
            continue;
        }
        let view_columns = columns.entry((schema.clone(), name.clone())).or_default();
        let Some(column) = row.get::<Option<String>, _>("column_name") else {
            continue;
        };
        let object = format!("{name}.{column}");
        let column_type = postgres_type(&row.get::<String, _>("type_name"))
            .map_err(|error| ir_error(&schema, &object, &error))?;
        view_columns.push(ViewColumn::new(column, column_type));
    }
    let mut base_tables = BTreeMap::<TableKey, Vec<ViewBaseTable>>::new();
    for row in sqlx::query(AssertSqlSafe((VIEW_BASE_TABLES_SQL).to_owned()).into_sql_str())
        .bind(schemas)
        .fetch_all(client)
        .await
        .map_err(|error| database_error("query view base tables", error))?
    {
        let schema = row.get::<String, _>("view_schema");
        let name = row.get::<String, _>("view_name");
        if relation_is_excluded(excluded_relations, &schema, &name) {
            continue;
        }
        let base_schema = row.get::<String, _>("base_schema");
        let base_name = row.get::<String, _>("base_name");
        base_tables
            .entry((schema, name))
            .or_default()
            .push(ViewBaseTable::new(base_schema, base_name));
    }
    let mut views = Vec::new();
    for ((schema, name), columns) in columns {
        let row = sqlx::query(
            AssertSqlSafe(
                ("SELECT pg_catalog.pg_get_viewdef(rewrite.ev_class, false),
                    rewrite.ev_action::text LIKE '%:hasTargetSRFs true%' AS target_srfs
               FROM pg_catalog.pg_rewrite AS rewrite
               JOIN pg_catalog.pg_class AS relation ON relation.oid = rewrite.ev_class
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace
              WHERE namespace.nspname = $1 AND relation.relname = $2
                AND rewrite.rulename = '_RETURN'")
                    .to_owned(),
            )
            .into_sql_str(),
        )
        .bind(&schema)
        .bind(&name)
        .fetch_one(client)
        .await
        .map_err(|error| database_error("read the view definition", error))?;
        let definition: String = row.get(0);
        let target_srfs: bool = row.get(1);
        let statement = client
            .prepare(AssertSqlSafe(definition.clone()).into_sql_str())
            .await
            .map_err(|error| database_error("describe the view definition", error))?;
        let mut origins = Vec::new();
        for (column, described) in columns.into_iter().zip(statement.columns()) {
            let origin = match (described.relation_id(), described.relation_attribute_no()) {
                (Some(table), Some(number)) => sqlx::query(AssertSqlSafe(("SELECT namespace.nspname::text, relation.relname::text, attribute.attname::text
                       FROM pg_catalog.pg_attribute AS attribute
                       JOIN pg_catalog.pg_class AS relation ON relation.oid = attribute.attrelid
                       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                      WHERE attribute.attrelid = $1 AND attribute.attnum = $2").to_owned()).into_sql_str()).bind(table).bind(number).fetch_optional(client).await.map_err(|error| database_error("read a view column origin", error))?,
                _ => None,
            };
            origins.push(if let Some(origin) = origin {
                column.with_origin(ViewColumnOrigin {
                    schema: origin.get(0),
                    table: origin.get(1),
                    column: origin.get(2),
                })
            } else {
                column
            });
        }
        let dependencies = base_tables
            .remove(&(schema.clone(), name.clone()))
            .unwrap_or_default();
        views.push(
            View::new(schema, name, origins, dependencies).with_definition(definition, target_srfs),
        );
    }
    Ok(views)
}

async fn load_relations(
    client: &Client,
    schemas: &[String],
) -> Result<Vec<RelationRow>, PostgresIntrospectionError> {
    sqlx::query(AssertSqlSafe((RELATIONS_SQL).to_owned()).into_sql_str())
        .bind(schemas)
        .fetch_all(client)
        .await
        .map_err(|error| database_error("query configured-schema relations", error))
        .map(|rows| rows.iter().map(relation_row).collect())
}

fn relation_row(row: &Row) -> RelationRow {
    RelationRow {
        schema: row.get("schema_name"),
        name: row.get("relation_name"),
    }
}

fn model_relations(relations: &[RelationRow]) -> BTreeMap<TableKey, TableParts> {
    relations
        .iter()
        .map(|relation| {
            (
                (relation.schema.clone(), relation.name.clone()),
                TableParts {
                    columns: Vec::new(),
                    constraints: Vec::new(),
                    indexes: Vec::new(),
                    exclusions: Vec::new(),
                },
            )
        })
        .collect()
}

async fn load_columns(
    client: &Client,
    schemas: &[String],
) -> Result<Vec<ColumnRow>, PostgresIntrospectionError> {
    sqlx::query(AssertSqlSafe((COLUMNS_SQL).to_owned()).into_sql_str())
        .bind(schemas)
        .fetch_all(client)
        .await
        .map_err(|error| database_error("query configured-schema columns", error))
        .map(|rows| rows.iter().map(column_row).collect())
}

fn column_row(row: &Row) -> ColumnRow {
    ColumnRow {
        schema: row.get("schema_name"),
        table: row.get("table_name"),
        number: row.get("column_number"),
        name: row.get("column_name"),
        type_name: row.get("type_name"),
        nullable: row.get("nullable"),
        default_expression: row.get("default_expression"),
        identity: row.get("identity_kind"),
        generated: row.get("generated_kind"),
    }
}

fn map_columns(
    rows: &[&ColumnRow],
    tables: &mut BTreeMap<TableKey, TableParts>,
) -> Result<BTreeMap<AttributeKey, String>, PostgresIntrospectionError> {
    let mut attributes = BTreeMap::new();
    for row in rows {
        let object = format!("{}.{}", row.table, row.name);
        let column_type = postgres_type(&row.type_name)
            .map_err(|error| ir_error(&row.schema, &object, &error))?;
        let (default, generation) = match (row.identity.as_str(), row.generated.as_str()) {
            ("", "") => {
                let default = row
                    .default_expression
                    .as_deref()
                    .map(|expression| postgres_default(column_type, expression))
                    .transpose()
                    .map_err(|error| ir_error(&row.schema, &object, &error))?;
                (default, None)
            }
            ("a", "") => {
                if row.nullable {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedIdentity,
                        Some(&row.schema),
                        Some(&object),
                        "identity columns must be NOT NULL",
                    ));
                }
                (
                    None,
                    Some(ColumnGeneration::Identity {
                        mode: IdentityMode::Always,
                    }),
                )
            }
            ("d", "") => {
                if row.nullable {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedIdentity,
                        Some(&row.schema),
                        Some(&object),
                        "identity columns must be NOT NULL",
                    ));
                }
                (
                    None,
                    Some(ColumnGeneration::Identity {
                        mode: IdentityMode::ByDefault,
                    }),
                )
            }
            ("", "s") => {
                let Some(expression) = &row.default_expression else {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedGeneratedColumn,
                        Some(&row.schema),
                        Some(&object),
                        "stored generated column has no pg_attrdef expression",
                    ));
                };
                (None, Some(ColumnGeneration::stored(expression.clone())))
            }
            ("", "v") => {
                return Err(refusal(
                    PostgresIntrospectionErrorType::UnsupportedGeneratedColumn,
                    Some(&row.schema),
                    Some(&object),
                    "virtual generated columns are not supported",
                ));
            }
            (identity, generated) => {
                return Err(refusal(
                    PostgresIntrospectionErrorType::UnsupportedGeneratedColumn,
                    Some(&row.schema),
                    Some(&object),
                    format!(
                        "unsupported pg_attribute generation shape identity=`{identity}` generated=`{generated}`"
                    ),
                ));
            }
        };

        let key = (row.schema.clone(), row.table.clone());
        let Some(table) = tables.get_mut(&key) else {
            return Err(refusal(
                PostgresIntrospectionErrorType::UnsupportedRelation,
                Some(&row.schema),
                Some(&row.table),
                "column belongs to a relation that is not an ordinary table",
            ));
        };
        table.columns.push(Column::new(
            row.name.clone(),
            column_type,
            row.nullable,
            default,
            generation,
        ));
        attributes.insert(
            (row.schema.clone(), row.table.clone(), row.number),
            row.name.clone(),
        );
    }
    Ok(attributes)
}

async fn load_constraints(
    client: &Client,
    schemas: &[String],
) -> Result<Vec<ConstraintRow>, PostgresIntrospectionError> {
    sqlx::query(AssertSqlSafe((CONSTRAINTS_SQL).to_owned()).into_sql_str())
        .bind(schemas)
        .fetch_all(client)
        .await
        .map_err(|error| database_error("query configured-schema constraints", error))
        .map(|rows| rows.iter().map(constraint_row).collect())
}

fn constraint_row(row: &Row) -> ConstraintRow {
    ConstraintRow {
        schema: row.get("schema_name"),
        table: row.get("table_name"),
        name: row.get("constraint_name"),
        type_: row.get("constraint_kind"),
        columns: row.get("column_numbers"),
        referenced_schema: row.get("referenced_schema"),
        referenced_table: row.get("referenced_table"),
        referenced_columns: row.get("referenced_column_numbers"),
        on_update: row.get("on_update"),
        on_delete: row.get("on_delete"),
        match_type: row.get("match_type"),
        expression: row.get("check_expression"),
        deferrable: row.get("deferrable"),
        initially_deferred: row.get("initially_deferred"),
        enforced: row.get("enforced"),
        validated: row.get("validated"),
        local: row.get("is_local"),
        inherited_count: row.get("inherited_count"),
        no_inherit: row.get("no_inherit"),
        period: row.get("is_period"),
        parent: row.get("has_parent"),
        delete_column_subset: row.get("has_delete_column_subset"),
        supporting_index_method: row.get("supporting_index_method"),
        supporting_index_keys: row.get("supporting_index_keys"),
        supporting_index_attributes: row.get("supporting_index_attributes"),
        supporting_index_nulls_not_distinct: row.get("supporting_index_nulls_not_distinct"),
        supporting_index_default_operator_classes: row
            .get("supporting_index_has_default_operator_classes"),
        supporting_index_default_collations: row.get("supporting_index_has_default_collations"),
        supporting_index_expression: row.get("supporting_index_has_expression"),
        supporting_index_predicate: row.get("supporting_index_has_predicate"),
        exclusion_operators: row.get("exclusion_operators"),
        exclusion_key_definitions: row.get("exclusion_key_definitions"),
        exclusion_dependency_columns: row.get("exclusion_dependency_columns"),
    }
}

async fn load_indexes(
    client: &Client,
    schemas: &[String],
) -> Result<Vec<IndexRow>, PostgresIntrospectionError> {
    sqlx::query(AssertSqlSafe((INDEXES_SQL).to_owned()).into_sql_str())
        .bind(schemas)
        .fetch_all(client)
        .await
        .map_err(|error| database_error("query ordinary indexes", error))
        .map(|rows| rows.iter().map(index_row).collect())
}

fn index_row(row: &Row) -> IndexRow {
    IndexRow {
        schema: row.get("schema_name"),
        table: row.get("table_name"),
        name: row.get("index_name"),
        access_method: row.get("access_method"),
        unique: row.get("is_unique"),
        nulls_not_distinct: row.get("nulls_not_distinct"),
        exclusion: row.get("is_exclusion"),
        immediate: row.get("is_immediate"),
        valid: row.get("is_valid"),
        ready: row.get("is_ready"),
        live: row.get("is_live"),
        attributes: row.get("attribute_count"),
        keys: row.get("key_count"),
        column_numbers: row.get("column_numbers"),
        options: row.get("key_options"),
        default_operator_classes: row.get("default_operator_classes"),
        default_collations: row.get("default_collations"),
        expression: row.get("has_expression"),
        predicate: row.get("has_predicate"),
    }
}

fn names_for_attributes(
    type_: PostgresIntrospectionErrorType,
    schema: &str,
    table: &str,
    object: &str,
    numbers: &[i16],
    attributes: &BTreeMap<AttributeKey, String>,
) -> Result<Vec<String>, PostgresIntrospectionError> {
    numbers
        .iter()
        .map(|number| {
            attributes
                .get(&(schema.to_owned(), table.to_owned(), *number))
                .cloned()
                .ok_or_else(|| {
                    refusal(
                        type_,
                        Some(schema),
                        Some(object),
                        format!("catalog key references unsupported attribute number `{number}`"),
                    )
                })
        })
        .collect()
}

/// Name an authored object from columns in table-definition order.
pub fn authored_object_name(table: &str, columns: &[String], suffix: &str) -> String {
    if columns.is_empty() {
        format!("{table}_{suffix}")
    } else {
        format!("{table}_{}_{suffix}", columns.join("_"))
    }
}

fn validate_authored_name(
    type_: PostgresIntrospectionErrorType,
    schema: &str,
    table: &str,
    name: &str,
    suffix: &str,
    column_numbers: &[i16],
    attributes: &BTreeMap<AttributeKey, String>,
) -> Result<(), PostgresIntrospectionError> {
    if name.is_empty() {
        return Err(refusal(
            type_,
            Some(schema),
            Some(table),
            "catalog object name is empty",
        ));
    }

    let mut ordered_numbers = column_numbers.to_vec();
    ordered_numbers.sort_unstable();
    ordered_numbers.dedup();
    let columns = names_for_attributes(type_, schema, table, name, &ordered_numbers, attributes)?;
    let expected = authored_object_name(table, &columns, suffix);

    // PostgreSQL truncates identifiers at 63 bytes. The pre-apply validator
    // requires an explicit shorter name when the convention cannot fit, but
    // pg_catalog cannot reconstruct that authored spelling.
    if expected.len() < 64 && name != expected {
        return Err(refusal(
            type_,
            Some(schema),
            Some(name),
            format!("name must use the authored convention `{expected}`"),
        ));
    }
    Ok(())
}

fn validate_constraint_shape(row: &ConstraintRow) -> Result<(), PostgresIntrospectionError> {
    if row.deferrable
        || row.initially_deferred
        || !row.enforced
        || !row.validated
        || !row.local
        || row.inherited_count != 0
        || (matches!(row.type_.as_str(), "c" | "n") && row.no_inherit)
        || row.period
        || row.parent
    {
        return Err(refusal(
            PostgresIntrospectionErrorType::UnsupportedConstraint,
            Some(&row.schema),
            Some(&row.name),
            "deferred, unenforced, unvalidated, inherited, NO INHERIT check/not-null, and temporal constraint shapes are not supported",
        ));
    }
    Ok(())
}

fn validate_constraint_index(row: &ConstraintRow) -> Result<(), PostgresIntrospectionError> {
    let supporting_column_count = i16::try_from(row.columns.len()).map_err(|_| {
        refusal(
            PostgresIntrospectionErrorType::UnsupportedConstraint,
            Some(&row.schema),
            Some(&row.name),
            "constraint has more columns than PostgreSQL can represent",
        )
    })?;
    if row.supporting_index_method.as_deref() != Some("btree")
        || row.supporting_index_keys != Some(supporting_column_count)
        || row.supporting_index_attributes != row.supporting_index_keys
        || row.supporting_index_nulls_not_distinct != Some(false)
        || !row.supporting_index_default_operator_classes
        || !row.supporting_index_default_collations
        || row.supporting_index_expression
        || row.supporting_index_predicate
    {
        return Err(refusal(
            PostgresIntrospectionErrorType::UnsupportedConstraint,
            Some(&row.schema),
            Some(&row.name),
            "primary and unique constraints require a plain btree index over exactly their named columns",
        ));
    }
    Ok(())
}

fn foreign_key_action(
    row: &ConstraintRow,
    action: &str,
) -> Result<ForeignKeyAction, PostgresIntrospectionError> {
    match action {
        "a" => Ok(ForeignKeyAction::NoAction),
        "r" => Ok(ForeignKeyAction::Restrict),
        "c" => Ok(ForeignKeyAction::Cascade),
        "n" => Ok(ForeignKeyAction::SetNull),
        "d" => Ok(ForeignKeyAction::SetDefault),
        unsupported => Err(refusal(
            PostgresIntrospectionErrorType::UnsupportedConstraint,
            Some(&row.schema),
            Some(&row.name),
            format!("unsupported foreign-key action `{unsupported}`"),
        )),
    }
}

/// Model one `EXCLUDE` constraint from its `pg_constraint` row.
///
/// The supporting gist index carries the shape: `conkey` holds one entry per key
/// in index order, `0` where the key is an expression, and `conexclop` holds the
/// matching operator. Unlike a primary or unique constraint, an expression key
/// is admitted, because the frozen column vocabulary has no range type and the
/// non-overlap invariant is written as `tstzrange(a, b) WITH &&`.
///
/// The authored-name convention binds wherever it can be reconstructed. When
/// every key is a column, the name must read `{table}_{columns}_excl`, which is
/// PostgreSQL's own default spelling. THE ONE EXEMPTION IS AN EXPRESSION KEY:
/// `validate_authored_name` rebuilds the expected name from column names, and an
/// expression has none, so a constraint carrying one is required only to be
/// named. The rule is not dropped, the exemption is stated.
fn map_exclusion(
    row: &ConstraintRow,
    attributes: &BTreeMap<AttributeKey, String>,
) -> Result<Exclusion, PostgresIntrospectionError> {
    let refuse = |detail: String| {
        refusal(
            PostgresIntrospectionErrorType::UnsupportedConstraint,
            Some(&row.schema),
            Some(&row.name),
            detail,
        )
    };

    if row.supporting_index_method.as_deref() != Some("gist") {
        return Err(refuse(format!(
            "exclusion constraints require a gist index, found `{}`",
            row.supporting_index_method.as_deref().unwrap_or("none")
        )));
    }
    let key_count = i16::try_from(row.columns.len())
        .map_err(|_| refuse("constraint has more keys than PostgreSQL can represent".to_owned()))?;
    if key_count == 0
        || row.exclusion_operators.len() != row.columns.len()
        || row.exclusion_key_definitions.len() != row.columns.len()
        || row.supporting_index_keys != Some(key_count)
        || row.supporting_index_attributes != row.supporting_index_keys
        || !row.supporting_index_default_operator_classes
        || !row.supporting_index_default_collations
        || row.supporting_index_predicate
    {
        return Err(refuse(
            "exclusion constraints require one operator per key over a total gist index with \
             default operator classes and collations"
                .to_owned(),
        ));
    }
    if !row.columns.contains(&0) {
        validate_authored_name(
            PostgresIntrospectionErrorType::UnsupportedConstraint,
            &row.schema,
            &row.table,
            &row.name,
            "excl",
            &row.columns,
            attributes,
        )?;
    }

    let mut keys = Vec::with_capacity(row.columns.len());
    for ((number, definition), operator) in row
        .columns
        .iter()
        .zip(&row.exclusion_key_definitions)
        .zip(&row.exclusion_operators)
    {
        let element = if *number == 0 {
            ExclusionElement::expression(definition.clone())
        } else {
            let name = names_for_attributes(
                PostgresIntrospectionErrorType::UnsupportedConstraint,
                &row.schema,
                &row.table,
                &row.name,
                std::slice::from_ref(number),
                attributes,
            )?
            .pop()
            .expect("one attribute number resolves to one column name");
            ExclusionElement::column(name)
        };
        keys.push(ExclusionKey::new(element, operator.clone()));
    }

    // PostgreSQL records an auto dependency from the constraint to each plain
    // key column and from its index to each column an expression key reads --
    // the dependency that blocks `DROP COLUMN`. Reading it is how the column
    // behind `tstzrange(starts_at, ends_at)` is known without parsing SQL.
    let columns = names_for_attributes(
        PostgresIntrospectionErrorType::UnsupportedConstraint,
        &row.schema,
        &row.table,
        &row.name,
        &row.exclusion_dependency_columns,
        attributes,
    )?;
    if !row
        .columns
        .iter()
        .all(|number| *number == 0 || row.exclusion_dependency_columns.contains(number))
    {
        return Err(refuse(
            "exclusion constraint depends on fewer columns than it keys".to_owned(),
        ));
    }

    Exclusion::new(row.name.clone(), ExclusionAccessMethod::Gist, keys, columns)
        .map_err(|error| ir_error(&row.schema, &row.name, &error))
}

fn table_parts<'a>(
    tables: &'a mut BTreeMap<TableKey, TableParts>,
    row: &ConstraintRow,
) -> Result<&'a mut TableParts, PostgresIntrospectionError> {
    tables
        .get_mut(&(row.schema.clone(), row.table.clone()))
        .ok_or_else(|| {
            refusal(
                PostgresIntrospectionErrorType::UnsupportedConstraint,
                Some(&row.schema),
                Some(&row.name),
                "constraint belongs to a relation that is not an ordinary table",
            )
        })
}

fn platform_reference_is_permitted(
    schema: &str,
    table: &str,
    columns: &[i16],
    attributes: &BTreeMap<AttributeKey, String>,
) -> bool {
    schema == "platform"
        && matches!(table, "external_system" | "file")
        && columns.len() == 1
        && attributes
            .get(&(schema.to_owned(), table.to_owned(), columns[0]))
            .is_some_and(|name| name == "id")
}

fn map_constraints(
    rows: &[ConstraintRow],
    attributes: &BTreeMap<AttributeKey, String>,
    tables: &mut BTreeMap<TableKey, TableParts>,
) -> Result<(), PostgresIntrospectionError> {
    for row in rows {
        validate_constraint_shape(row)?;
        if row.type_ == "n" {
            continue;
        }
        if row.type_ == "x" {
            let exclusion = map_exclusion(row, attributes)?;
            table_parts(tables, row)?.exclusions.push(exclusion);
            continue;
        }

        let columns = names_for_attributes(
            PostgresIntrospectionErrorType::UnsupportedConstraint,
            &row.schema,
            &row.table,
            &row.name,
            &row.columns,
            attributes,
        )?;
        let constraint = match row.type_.as_str() {
            "p" => {
                if columns.is_empty() {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "primary key has no columns",
                    ));
                }
                validate_authored_name(
                    PostgresIntrospectionErrorType::UnsupportedConstraint,
                    &row.schema,
                    &row.table,
                    &row.name,
                    "pkey",
                    &row.columns,
                    attributes,
                )?;
                validate_constraint_index(row)?;
                Constraint::primary_key(row.name.clone(), columns)
                    .map_err(|error| ir_error(&row.schema, &row.name, &error))?
            }
            "u" => {
                if columns.is_empty() {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "unique constraint has no columns",
                    ));
                }
                validate_authored_name(
                    PostgresIntrospectionErrorType::UnsupportedConstraint,
                    &row.schema,
                    &row.table,
                    &row.name,
                    "key",
                    &row.columns,
                    attributes,
                )?;
                validate_constraint_index(row)?;
                Constraint::unique(row.name.clone(), columns)
                    .map_err(|error| ir_error(&row.schema, &row.name, &error))?
            }
            "f" => {
                if columns.is_empty() || columns.len() != row.referenced_columns.len() {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "foreign key has missing or mismatched column pairs",
                    ));
                }
                if row.match_type != "s" || row.delete_column_subset {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "only MATCH SIMPLE foreign keys acting on every local column are supported",
                    ));
                }
                let Some(referenced_schema) = &row.referenced_schema else {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "foreign key has no referenced schema",
                    ));
                };
                let Some(referenced_table) = &row.referenced_table else {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "foreign key has no referenced table",
                    ));
                };
                let provenance = platform_reference_is_permitted(
                    referenced_schema,
                    referenced_table,
                    &row.referenced_columns,
                    attributes,
                );
                if !provenance
                    && !attributes.keys().any(|(schema, table, _)| {
                        schema == referenced_schema && table == referenced_table
                    })
                {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "foreign keys must reference an ordinary table in the configured schemas",
                    ));
                }
                let referenced_columns = names_for_attributes(
                    PostgresIntrospectionErrorType::UnsupportedConstraint,
                    referenced_schema,
                    referenced_table,
                    &row.name,
                    &row.referenced_columns,
                    attributes,
                )?;
                validate_authored_name(
                    PostgresIntrospectionErrorType::UnsupportedConstraint,
                    &row.schema,
                    &row.table,
                    &row.name,
                    "fkey",
                    &row.columns,
                    attributes,
                )?;
                let pairs = columns
                    .into_iter()
                    .zip(referenced_columns)
                    .map(|(column, referenced)| ForeignKeyColumn::new(column, referenced))
                    .collect();
                Constraint::foreign_key(
                    row.name.clone(),
                    pairs,
                    referenced_schema.clone(),
                    referenced_table.clone(),
                    foreign_key_action(row, &row.on_update)?,
                    foreign_key_action(row, &row.on_delete)?,
                )
                .map_err(|error| ir_error(&row.schema, &row.name, &error))?
            }
            "c" => {
                let Some(expression) = &row.expression else {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "check constraint has no server expression",
                    ));
                };
                validate_authored_name(
                    PostgresIntrospectionErrorType::UnsupportedConstraint,
                    &row.schema,
                    &row.table,
                    &row.name,
                    "check",
                    &row.columns,
                    attributes,
                )?;
                Constraint::check(row.name.clone(), expression.clone())
                    .map_err(|error| ir_error(&row.schema, &row.name, &error))?
            }
            unsupported => {
                return Err(refusal(
                    PostgresIntrospectionErrorType::UnsupportedConstraint,
                    Some(&row.schema),
                    Some(&row.name),
                    format!("unsupported pg_constraint contype `{unsupported}`"),
                ));
            }
        };

        table_parts(tables, row)?.constraints.push(constraint);
    }
    Ok(())
}

fn map_indexes(
    rows: &[IndexRow],
    attributes: &BTreeMap<AttributeKey, String>,
    tables: &mut BTreeMap<TableKey, TableParts>,
) -> Result<(), PostgresIntrospectionError> {
    for row in rows {
        let key_count = usize::try_from(row.keys).unwrap_or(0);
        let distinct_column_count = row
            .column_numbers
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len();
        if row.access_method != "btree"
            || row.unique
            || row.nulls_not_distinct
            || row.exclusion
            || !row.immediate
            || !row.valid
            || !row.ready
            || !row.live
            || row.keys <= 0
            || row.attributes != row.keys
            || row.expression
            || row.predicate
            || row.column_numbers.len() != key_count
            || distinct_column_count != key_count
            || row.options.len() != key_count
            || row.default_operator_classes.len() != key_count
            || row.default_collations.len() != key_count
            || row.default_operator_classes.contains(&false)
            || row.default_collations.contains(&false)
        {
            return Err(refusal(
                PostgresIntrospectionErrorType::UnsupportedIndex,
                Some(&row.schema),
                Some(&row.name),
                "only valid non-unique btree indexes over distinct named columns with default operator classes, collations, and null ordering are supported",
            ));
        }

        validate_authored_name(
            PostgresIntrospectionErrorType::UnsupportedIndex,
            &row.schema,
            &row.table,
            &row.name,
            "idx",
            &row.column_numbers,
            attributes,
        )?;
        let names = names_for_attributes(
            PostgresIntrospectionErrorType::UnsupportedIndex,
            &row.schema,
            &row.table,
            &row.name,
            &row.column_numbers,
            attributes,
        )?;
        let mut columns = Vec::with_capacity(names.len());
        for (name, option) in names.into_iter().zip(&row.options) {
            let direction = match option {
                0 => IndexDirection::Asc,
                3 => IndexDirection::Desc,
                unsupported => {
                    return Err(refusal(
                        PostgresIntrospectionErrorType::UnsupportedIndex,
                        Some(&row.schema),
                        Some(&row.name),
                        format!("unsupported btree ordering option `{unsupported}`"),
                    ));
                }
            };
            columns.push(IndexColumn::new(name, direction));
        }
        let index = Index::new(row.name.clone(), columns).map_err(|error| {
            refusal(
                PostgresIntrospectionErrorType::UnsupportedIndex,
                Some(&row.schema),
                Some(&row.name),
                error.to_string(),
            )
        })?;
        let Some(table) = tables.get_mut(&(row.schema.clone(), row.table.clone())) else {
            return Err(refusal(
                PostgresIntrospectionErrorType::UnsupportedIndex,
                Some(&row.schema),
                Some(&row.name),
                "index belongs to a relation that is not an ordinary table",
            ));
        };
        table.indexes.push(index);
    }
    Ok(())
}

/// Read explicitly configured application schemas from PostgreSQL 18.
pub async fn read_catalog(
    client: &Client,
    application_schemas: &[&str],
) -> Result<CatalogIr, PostgresIntrospectionError> {
    read_catalog_excluding_relations(client, application_schemas, &[]).await
}

/// Read configured application schemas while omitting explicitly named
/// host-owned relations from the package IR.
///
/// The reader skips history tables and the host-owned relations that the caller names.
pub async fn read_catalog_excluding_relations(
    client: &Client,
    application_schemas: &[&str],
    excluded_relations: &[(&str, &str)],
) -> Result<CatalogIr, PostgresIntrospectionError> {
    let schemas = configured_schemas(application_schemas);
    if schemas.is_empty() {
        return Ok(CatalogIr::new(Vec::new()));
    }

    validate_schemas(client, &schemas).await?;
    let relations = load_relations(client, &schemas)
        .await?
        .into_iter()
        .filter(|relation| {
            !relation_is_excluded(excluded_relations, &relation.schema, &relation.name)
        })
        .collect::<Vec<_>>();
    let views = load_views(client, &schemas, excluded_relations).await?;
    let mut tables = model_relations(&relations);

    let all_columns = load_columns(client, &schemas).await?;
    let columns = all_columns
        .iter()
        .filter(|column| !relation_is_excluded(excluded_relations, &column.schema, &column.table))
        .collect::<Vec<_>>();
    let mut attributes = map_columns(&columns, &mut tables)?;
    for column in all_columns
        .iter()
        .filter(|column| relation_is_excluded(excluded_relations, &column.schema, &column.table))
    {
        attributes.insert(
            (column.schema.clone(), column.table.clone(), column.number),
            column.name.clone(),
        );
    }
    // These platform identity keys are public references outside application schemas.
    for row in sqlx::query(AssertSqlSafe(("SELECT c.relname::text, a.attnum FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid = a.attrelid JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'platform' AND c.relname IN ('external_system', 'file') AND c.relkind = 'r' AND a.attname = 'id' AND a.atttypid = 'uuid'::regtype AND NOT a.attisdropped").to_owned()).into_sql_str()).fetch_all(client).await.map_err(|error| database_error("read the platform reference keys", error))? {
        attributes.insert(("platform".into(), row.get(0), row.get(1)), "id".into());
    }
    let all_constraints = load_constraints(client, &schemas).await?;
    let mut referencing_constraints = Vec::new();
    for row in &all_constraints {
        if row.type_ != "f"
            || !relation_is_excluded(excluded_relations, &row.schema, &row.table)
            || !row
                .referenced_schema
                .as_ref()
                .zip(row.referenced_table.as_ref())
                .is_some_and(|(schema, table)| {
                    tables.contains_key(&(schema.clone(), table.clone()))
                })
        {
            continue;
        }
        let columns = names_for_attributes(
            PostgresIntrospectionErrorType::UnsupportedConstraint,
            &row.schema,
            &row.table,
            &row.name,
            &row.columns,
            &attributes,
        )?;
        let schema = row
            .referenced_schema
            .as_deref()
            .expect("selected foreign key target");
        let table = row
            .referenced_table
            .as_deref()
            .expect("selected foreign key target");
        let referenced = names_for_attributes(
            PostgresIntrospectionErrorType::UnsupportedConstraint,
            schema,
            table,
            &row.name,
            &row.referenced_columns,
            &attributes,
        )?;
        referencing_constraints.push(
            Constraint::foreign_key(
                row.name.clone(),
                columns
                    .into_iter()
                    .zip(referenced)
                    .map(|(column, referenced)| ForeignKeyColumn::new(column, referenced))
                    .collect(),
                schema,
                table,
                foreign_key_action(row, &row.on_update)?,
                foreign_key_action(row, &row.on_delete)?,
            )
            .map_err(|error| ir_error(&row.schema, &row.name, &error))?,
        );
    }
    let constraints = all_constraints
        .into_iter()
        .filter(|constraint| {
            !relation_is_excluded(excluded_relations, &constraint.schema, &constraint.table)
        })
        .collect::<Vec<_>>();
    map_constraints(&constraints, &attributes, &mut tables)?;
    let indexes = load_indexes(client, &schemas)
        .await?
        .into_iter()
        .filter(|index| !relation_is_excluded(excluded_relations, &index.schema, &index.table))
        .collect::<Vec<_>>();
    map_indexes(&indexes, &attributes, &mut tables)?;

    Ok(CatalogIr::new(
        tables
            .into_iter()
            .map(|((schema, name), parts)| {
                Table::new(
                    schema,
                    name,
                    parts.columns,
                    parts.constraints,
                    parts.indexes,
                )
                .with_exclusions(parts.exclusions)
            })
            .collect(),
    )
    .with_views(views)
    .with_referencing_constraints(referencing_constraints))
}

#[cfg(test)]
mod file_reference_tests {
    use super::*;

    #[test]
    fn only_file_and_external_system_identity_keys_are_public_references() {
        let mut attributes = BTreeMap::new();
        for table in ["file", "external_system", "connection"] {
            attributes.insert(
                ("platform".to_owned(), table.to_owned(), 1),
                "id".to_owned(),
            );
            attributes.insert(
                ("platform".to_owned(), table.to_owned(), 2),
                "name".to_owned(),
            );
        }
        for table in ["file", "external_system"] {
            assert!(platform_reference_is_permitted(
                "platform",
                table,
                &[1],
                &attributes
            ));
            assert!(!platform_reference_is_permitted(
                "platform",
                table,
                &[2],
                &attributes
            ));
            assert!(!platform_reference_is_permitted(
                "platform",
                table,
                &[1, 2],
                &attributes
            ));
            assert!(!platform_reference_is_permitted(
                "platform",
                table,
                &[],
                &attributes
            ));
            assert!(!platform_reference_is_permitted(
                "other",
                table,
                &[1],
                &attributes
            ));
        }
        assert!(!platform_reference_is_permitted(
            "platform",
            "connection",
            &[1],
            &attributes
        ));
    }
}
