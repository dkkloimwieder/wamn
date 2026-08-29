//! PostgreSQL 18 `pg_catalog` reader for the normalized schema IR.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use tokio_postgres::{Client, Row};

use crate::ir::{
    CatalogIr, Column, ColumnGeneration, Constraint, ForeignKeyAction, ForeignKeyColumn,
    IdentityMode, Index, IndexColumn, IndexDirection, IrError, IrErrorKind, Table,
    postgres_default, postgres_type,
};

/// Stable class of PostgreSQL catalog refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostgresIntrospectionErrorKind {
    Database,
    MissingSchema,
    UnsupportedTable,
    UnsupportedView,
    UnsupportedMaterializedView,
    UnsupportedForeignTable,
    UnsupportedRelation,
    UnsupportedRoutine,
    UnsupportedTrigger,
    UnsupportedRule,
    UnsupportedPolicy,
    UnsupportedCustomType,
    UnsupportedAcl,
    UnsupportedColumnType,
    UnsupportedColumnCollation,
    UnsupportedColumnDefault,
    UnsupportedGeneratedColumn,
    UnsupportedIdentity,
    UnsupportedConstraint,
    UnsupportedIndex,
    UnsupportedSequence,
}

impl PostgresIntrospectionErrorKind {
    const fn code(self) -> &'static str {
        match self {
            Self::Database => "database",
            Self::MissingSchema => "missing-schema",
            Self::UnsupportedTable => "unsupported-table",
            Self::UnsupportedView => "unsupported-view",
            Self::UnsupportedMaterializedView => "unsupported-materialized-view",
            Self::UnsupportedForeignTable => "unsupported-foreign-table",
            Self::UnsupportedRelation => "unsupported-relation",
            Self::UnsupportedRoutine => "unsupported-routine",
            Self::UnsupportedTrigger => "unsupported-trigger",
            Self::UnsupportedRule => "unsupported-rule",
            Self::UnsupportedPolicy => "unsupported-policy",
            Self::UnsupportedCustomType => "unsupported-custom-type",
            Self::UnsupportedAcl => "unsupported-acl",
            Self::UnsupportedColumnType => "unsupported-column-type",
            Self::UnsupportedColumnCollation => "unsupported-column-collation",
            Self::UnsupportedColumnDefault => "unsupported-column-default",
            Self::UnsupportedGeneratedColumn => "unsupported-generated-column",
            Self::UnsupportedIdentity => "unsupported-identity",
            Self::UnsupportedConstraint => "unsupported-constraint",
            Self::UnsupportedIndex => "unsupported-index",
            Self::UnsupportedSequence => "unsupported-sequence",
        }
    }
}

/// Contextual refusal at the PostgreSQL catalog boundary.
#[derive(Debug)]
pub struct PostgresIntrospectionError {
    kind: PostgresIntrospectionErrorKind,
    schema: Option<Box<str>>,
    object: Option<Box<str>>,
    detail: Box<str>,
    source: Option<tokio_postgres::Error>,
}

impl PostgresIntrospectionError {
    /// Stable refusal class.
    pub const fn kind(&self) -> PostgresIntrospectionErrorKind {
        self.kind
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
            self.kind.code()
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
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the row mirrors independent PostgreSQL pg_class flags"
)]
struct RelationRow {
    schema: String,
    name: String,
    kind: String,
    persistence: String,
    access_method: Option<String>,
    row_security: bool,
    force_row_security: bool,
    has_acl: bool,
    has_inheritance: bool,
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
    default_collation: bool,
    has_acl: bool,
}

#[derive(Debug)]
struct SequenceRow {
    schema: String,
    name: String,
    dependency: Option<String>,
    table_schema: Option<String>,
    table: Option<String>,
    column: Option<String>,
    column_identity: Option<String>,
    type_name: String,
    start: i64,
    increment: i64,
    maximum: i64,
    minimum: i64,
    cache: i64,
    cycle: bool,
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
    kind: String,
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

const SCHEMAS_SQL: &str = r"
WITH configured(schema_name) AS (
    SELECT unnest($1::text[])
)
SELECT configured.schema_name,
       namespace.oid IS NOT NULL AS present,
       namespace.nspacl IS NOT NULL AS has_acl
  FROM configured
  LEFT JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.nspname = configured.schema_name
 ORDER BY configured.schema_name
";

const RELATIONS_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       relation.relname::text AS relation_name,
       relation.relkind::text AS relation_kind,
       relation.relpersistence::text AS persistence,
       access_method.amname::text AS access_method,
       relation.relrowsecurity AS row_security,
       relation.relforcerowsecurity AS force_row_security,
       relation.relacl IS NOT NULL AS has_acl,
       EXISTS (
           SELECT 1
             FROM pg_catalog.pg_inherits AS inheritance
            WHERE inheritance.inhrelid = relation.oid
               OR inheritance.inhparent = relation.oid
       ) AS has_inheritance
  FROM pg_catalog.pg_class AS relation
  JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = relation.relnamespace
  LEFT JOIN pg_catalog.pg_am AS access_method
    ON access_method.oid = relation.relam
 WHERE namespace.nspname = ANY($1::text[])
 ORDER BY namespace.nspname, relation.relname, relation.relkind
";

const ROUTINES_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       routine.proname::text AS routine_name,
       routine.prokind::text AS routine_kind,
       pg_catalog.pg_get_function_identity_arguments(routine.oid) AS arguments
  FROM pg_catalog.pg_proc AS routine
  JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = routine.pronamespace
 WHERE namespace.nspname = ANY($1::text[])
 ORDER BY namespace.nspname, routine.proname,
          pg_catalog.pg_get_function_identity_arguments(routine.oid)
 LIMIT 1
";

const TRIGGERS_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       relation.relname::text AS table_name,
       trigger.tgname::text AS trigger_name
  FROM pg_catalog.pg_trigger AS trigger
  JOIN pg_catalog.pg_class AS relation
    ON relation.oid = trigger.tgrelid
  JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = relation.relnamespace
 WHERE namespace.nspname = ANY($1::text[])
   AND NOT trigger.tgisinternal
 ORDER BY namespace.nspname, relation.relname, trigger.tgname
 LIMIT 1
";

const RULES_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       relation.relname::text AS relation_name,
       rewrite.rulename::text AS rule_name
  FROM pg_catalog.pg_rewrite AS rewrite
  JOIN pg_catalog.pg_class AS relation
    ON relation.oid = rewrite.ev_class
  JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = relation.relnamespace
 WHERE namespace.nspname = ANY($1::text[])
   AND rewrite.rulename <> '_RETURN'
 ORDER BY namespace.nspname, relation.relname, rewrite.rulename
 LIMIT 1
";

const POLICIES_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       relation.relname::text AS table_name,
       policy.polname::text AS policy_name
  FROM pg_catalog.pg_policy AS policy
  JOIN pg_catalog.pg_class AS relation
    ON relation.oid = policy.polrelid
  JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = relation.relnamespace
 WHERE namespace.nspname = ANY($1::text[])
 ORDER BY namespace.nspname, relation.relname, policy.polname
 LIMIT 1
";

const TYPES_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       catalog_type.typname::text AS type_name,
       catalog_type.typtype::text AS type_kind,
       catalog_type.typacl IS NOT NULL AS has_acl,
       relation.relkind::text AS relation_kind,
       element_namespace.nspname::text AS element_schema,
       element_relation.relkind::text AS element_relation_kind
  FROM pg_catalog.pg_type AS catalog_type
  JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = catalog_type.typnamespace
  LEFT JOIN pg_catalog.pg_class AS relation
    ON relation.oid = catalog_type.typrelid
  LEFT JOIN pg_catalog.pg_type AS element_type
    ON element_type.oid = catalog_type.typelem
  LEFT JOIN pg_catalog.pg_namespace AS element_namespace
    ON element_namespace.oid = element_type.typnamespace
  LEFT JOIN pg_catalog.pg_class AS element_relation
    ON element_relation.oid = element_type.typrelid
 WHERE namespace.nspname = ANY($1::text[])
 ORDER BY namespace.nspname, catalog_type.typname
";

const DEFAULT_ACLS_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       owner.rolname::text AS owner_name,
       default_acl.defaclobjtype::text AS object_kind
  FROM pg_catalog.pg_default_acl AS default_acl
  JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = default_acl.defaclnamespace
  JOIN pg_catalog.pg_roles AS owner
    ON owner.oid = default_acl.defaclrole
 WHERE namespace.nspname = ANY($1::text[])
 ORDER BY namespace.nspname, owner.rolname, default_acl.defaclobjtype
 LIMIT 1
";

const COLUMNS_SQL: &str = r"
SELECT namespace.nspname::text AS schema_name,
       relation.relname::text AS table_name,
       attribute.attnum AS column_number,
       attribute.attname::text AS column_name,
       pg_catalog.format_type(attribute.atttypid, attribute.atttypmod) AS type_name,
       NOT attribute.attnotnull AS nullable,
       pg_catalog.pg_get_expr(attribute_default.adbin, attribute_default.adrelid, false)
           AS default_expression,
       attribute.attidentity::text AS identity_kind,
       attribute.attgenerated::text AS generated_kind,
       attribute.attcollation = catalog_type.typcollation AS has_default_collation,
       attribute.attacl IS NOT NULL AS has_acl
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

const SEQUENCES_SQL: &str = r"
SELECT sequence_namespace.nspname::text AS sequence_schema,
       sequence_relation.relname::text AS sequence_name,
       dependency.deptype::text AS dependency_kind,
       table_namespace.nspname::text AS table_schema,
       table_relation.relname::text AS table_name,
       attribute.attname::text AS column_name,
       attribute.attidentity::text AS column_identity,
       pg_catalog.format_type(sequence_data.seqtypid, NULL) AS type_name,
       sequence_data.seqstart AS sequence_start,
       sequence_data.seqincrement AS sequence_increment,
       sequence_data.seqmax AS sequence_maximum,
       sequence_data.seqmin AS sequence_minimum,
       sequence_data.seqcache AS sequence_cache,
       sequence_data.seqcycle AS sequence_cycle
  FROM pg_catalog.pg_class AS sequence_relation
  JOIN pg_catalog.pg_namespace AS sequence_namespace
    ON sequence_namespace.oid = sequence_relation.relnamespace
  JOIN pg_catalog.pg_sequence AS sequence_data
    ON sequence_data.seqrelid = sequence_relation.oid
  LEFT JOIN pg_catalog.pg_depend AS dependency
    ON dependency.classid = 'pg_catalog.pg_class'::pg_catalog.regclass
   AND dependency.objid = sequence_relation.oid
   AND dependency.objsubid = 0
   AND dependency.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass
   AND dependency.refobjsubid > 0
   AND dependency.deptype IN ('a', 'i')
  LEFT JOIN pg_catalog.pg_class AS table_relation
    ON table_relation.oid = dependency.refobjid
  LEFT JOIN pg_catalog.pg_namespace AS table_namespace
    ON table_namespace.oid = table_relation.relnamespace
  LEFT JOIN pg_catalog.pg_attribute AS attribute
    ON attribute.attrelid = dependency.refobjid
   AND attribute.attnum = dependency.refobjsubid
 WHERE sequence_relation.relkind = 'S'
   AND (sequence_namespace.nspname = ANY($1::text[])
        OR table_namespace.nspname = ANY($1::text[]))
 ORDER BY sequence_namespace.nspname, sequence_relation.relname
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
              AND NOT COALESCE(key_collation.collation_oid = attribute.attcollation, false)
       ) AS supporting_index_has_default_collations,
       supporting_index.indexprs IS NOT NULL AS supporting_index_has_expression,
       supporting_index.indpred IS NOT NULL AS supporting_index_has_predicate
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
    kind: PostgresIntrospectionErrorKind,
    schema: Option<&str>,
    object: Option<&str>,
    detail: impl Into<Box<str>>,
) -> PostgresIntrospectionError {
    PostgresIntrospectionError {
        kind,
        schema: schema.map(Into::into),
        object: object.map(Into::into),
        detail: detail.into(),
        source: None,
    }
}

fn database_error(
    context: &'static str,
    source: tokio_postgres::Error,
) -> PostgresIntrospectionError {
    PostgresIntrospectionError {
        kind: PostgresIntrospectionErrorKind::Database,
        schema: None,
        object: None,
        detail: context.into(),
        source: Some(source),
    }
}

fn ir_error(schema: &str, object: &str, error: &IrError) -> PostgresIntrospectionError {
    let kind = match error.kind() {
        IrErrorKind::EmptyName => PostgresIntrospectionErrorKind::UnsupportedConstraint,
        IrErrorKind::UnsupportedType => PostgresIntrospectionErrorKind::UnsupportedColumnType,
        IrErrorKind::UnsupportedDefault => PostgresIntrospectionErrorKind::UnsupportedColumnDefault,
    };
    refusal(kind, Some(schema), Some(object), error.to_string())
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
    let rows = client
        .query(SCHEMAS_SQL, &[&schemas])
        .await
        .map_err(|error| database_error("query configured schemas", error))?;

    for row in rows {
        let schema = row.get::<_, String>("schema_name");
        if !row.get::<_, bool>("present") {
            return Err(refusal(
                PostgresIntrospectionErrorKind::MissingSchema,
                Some(&schema),
                None,
                "configured application schema does not exist",
            ));
        }
        if row.get::<_, bool>("has_acl") {
            return Err(refusal(
                PostgresIntrospectionErrorKind::UnsupportedAcl,
                Some(&schema),
                None,
                "schema has an explicit ACL",
            ));
        }
    }
    Ok(())
}

async fn load_relations(
    client: &Client,
    schemas: &[String],
) -> Result<Vec<RelationRow>, PostgresIntrospectionError> {
    client
        .query(RELATIONS_SQL, &[&schemas])
        .await
        .map_err(|error| database_error("query configured-schema relations", error))
        .map(|rows| rows.iter().map(relation_row).collect())
}

fn relation_row(row: &Row) -> RelationRow {
    RelationRow {
        schema: row.get("schema_name"),
        name: row.get("relation_name"),
        kind: row.get("relation_kind"),
        persistence: row.get("persistence"),
        access_method: row.get("access_method"),
        row_security: row.get("row_security"),
        force_row_security: row.get("force_row_security"),
        has_acl: row.get("has_acl"),
        has_inheritance: row.get("has_inheritance"),
    }
}

fn validate_relations(
    relations: &[RelationRow],
) -> Result<BTreeMap<TableKey, TableParts>, PostgresIntrospectionError> {
    let mut tables = BTreeMap::new();
    for relation in relations {
        match relation.kind.as_str() {
            "r" => {
                if relation.persistence != "p" {
                    return Err(refusal(
                        PostgresIntrospectionErrorKind::UnsupportedTable,
                        Some(&relation.schema),
                        Some(&relation.name),
                        format!(
                            "ordinary tables must be permanent, found persistence `{}`",
                            relation.persistence
                        ),
                    ));
                }
                if relation.access_method.as_deref() != Some("heap") {
                    return Err(refusal(
                        PostgresIntrospectionErrorKind::UnsupportedTable,
                        Some(&relation.schema),
                        Some(&relation.name),
                        format!(
                            "ordinary tables must use heap access, found `{}`",
                            relation.access_method.as_deref().unwrap_or("none")
                        ),
                    ));
                }
                if relation.has_inheritance {
                    return Err(refusal(
                        PostgresIntrospectionErrorKind::UnsupportedTable,
                        Some(&relation.schema),
                        Some(&relation.name),
                        "table inheritance is not supported",
                    ));
                }
                if relation.row_security || relation.force_row_security {
                    return Err(refusal(
                        PostgresIntrospectionErrorKind::UnsupportedPolicy,
                        Some(&relation.schema),
                        Some(&relation.name),
                        "row-level security state is not supported",
                    ));
                }
                if relation.has_acl {
                    return Err(refusal(
                        PostgresIntrospectionErrorKind::UnsupportedAcl,
                        Some(&relation.schema),
                        Some(&relation.name),
                        "table has an explicit ACL",
                    ));
                }
                tables.insert(
                    (relation.schema.clone(), relation.name.clone()),
                    TableParts {
                        columns: Vec::new(),
                        constraints: Vec::new(),
                        indexes: Vec::new(),
                    },
                );
            }
            "i" | "S" | "c" => {}
            "v" => {
                return Err(refusal(
                    PostgresIntrospectionErrorKind::UnsupportedView,
                    Some(&relation.schema),
                    Some(&relation.name),
                    "views are outside the supported catalog set",
                ));
            }
            "m" => {
                return Err(refusal(
                    PostgresIntrospectionErrorKind::UnsupportedMaterializedView,
                    Some(&relation.schema),
                    Some(&relation.name),
                    "materialized views are outside the supported catalog set",
                ));
            }
            "f" => {
                return Err(refusal(
                    PostgresIntrospectionErrorKind::UnsupportedForeignTable,
                    Some(&relation.schema),
                    Some(&relation.name),
                    "foreign tables are outside the supported catalog set",
                ));
            }
            "p" => {
                return Err(refusal(
                    PostgresIntrospectionErrorKind::UnsupportedTable,
                    Some(&relation.schema),
                    Some(&relation.name),
                    "partitioned tables are outside the supported catalog set",
                ));
            }
            "I" => {
                return Err(refusal(
                    PostgresIntrospectionErrorKind::UnsupportedIndex,
                    Some(&relation.schema),
                    Some(&relation.name),
                    "partitioned indexes are outside the supported catalog set",
                ));
            }
            unsupported => {
                return Err(refusal(
                    PostgresIntrospectionErrorKind::UnsupportedRelation,
                    Some(&relation.schema),
                    Some(&relation.name),
                    format!("unsupported pg_class relkind `{unsupported}`"),
                ));
            }
        }
    }
    Ok(tables)
}

async fn refuse_routines(
    client: &Client,
    schemas: &[String],
) -> Result<(), PostgresIntrospectionError> {
    let row = client
        .query_opt(ROUTINES_SQL, &[&schemas])
        .await
        .map_err(|error| database_error("query configured-schema routines", error))?;
    let Some(row) = row else {
        return Ok(());
    };

    let schema = row.get::<_, String>("schema_name");
    let name = row.get::<_, String>("routine_name");
    let arguments = row.get::<_, String>("arguments");
    let routine_kind = row.get::<_, String>("routine_kind");
    let kind = match routine_kind.as_str() {
        "f" => "function",
        "p" => "procedure",
        "a" => "aggregate",
        "w" => "window function",
        other => other,
    };
    Err(refusal(
        PostgresIntrospectionErrorKind::UnsupportedRoutine,
        Some(&schema),
        Some(&format!("{name}({arguments})")),
        format!("{kind} is outside the supported catalog set"),
    ))
}

async fn refuse_triggers(
    client: &Client,
    schemas: &[String],
) -> Result<(), PostgresIntrospectionError> {
    let row = client
        .query_opt(TRIGGERS_SQL, &[&schemas])
        .await
        .map_err(|error| database_error("query configured-schema triggers", error))?;
    let Some(row) = row else {
        return Ok(());
    };
    let schema = row.get::<_, String>("schema_name");
    let table = row.get::<_, String>("table_name");
    let trigger = row.get::<_, String>("trigger_name");
    Err(refusal(
        PostgresIntrospectionErrorKind::UnsupportedTrigger,
        Some(&schema),
        Some(&format!("{table}.{trigger}")),
        "user-defined triggers are outside the supported catalog set",
    ))
}

async fn refuse_rules(
    client: &Client,
    schemas: &[String],
) -> Result<(), PostgresIntrospectionError> {
    let row = client
        .query_opt(RULES_SQL, &[&schemas])
        .await
        .map_err(|error| database_error("query configured-schema rules", error))?;
    let Some(row) = row else {
        return Ok(());
    };
    let schema = row.get::<_, String>("schema_name");
    let relation = row.get::<_, String>("relation_name");
    let rule = row.get::<_, String>("rule_name");
    Err(refusal(
        PostgresIntrospectionErrorKind::UnsupportedRule,
        Some(&schema),
        Some(&format!("{relation}.{rule}")),
        "rules are outside the supported catalog set",
    ))
}

async fn refuse_policies(
    client: &Client,
    schemas: &[String],
) -> Result<(), PostgresIntrospectionError> {
    let row = client
        .query_opt(POLICIES_SQL, &[&schemas])
        .await
        .map_err(|error| database_error("query configured-schema policies", error))?;
    let Some(row) = row else {
        return Ok(());
    };
    let schema = row.get::<_, String>("schema_name");
    let table = row.get::<_, String>("table_name");
    let policy = row.get::<_, String>("policy_name");
    Err(refusal(
        PostgresIntrospectionErrorKind::UnsupportedPolicy,
        Some(&schema),
        Some(&format!("{table}.{policy}")),
        "row-level security policies are outside the supported catalog set",
    ))
}

async fn validate_types(
    client: &Client,
    schemas: &[String],
) -> Result<(), PostgresIntrospectionError> {
    let rows = client
        .query(TYPES_SQL, &[&schemas])
        .await
        .map_err(|error| database_error("query configured-schema types", error))?;

    for row in rows {
        let schema = row.get::<_, String>("schema_name");
        let name = row.get::<_, String>("type_name");
        let relation_kind = row.get::<_, Option<String>>("relation_kind");
        let element_schema = row.get::<_, Option<String>>("element_schema");
        let element_relation_kind = row.get::<_, Option<String>>("element_relation_kind");
        let is_table_row = relation_kind.as_deref() == Some("r");
        let is_table_row_array = element_schema.as_deref() == Some(schema.as_str())
            && element_relation_kind.as_deref() == Some("r");
        let is_generated_local_array = element_schema.as_deref() == Some(schema.as_str());

        if is_table_row || is_table_row_array {
            if row.get::<_, bool>("has_acl") {
                return Err(refusal(
                    PostgresIntrospectionErrorKind::UnsupportedAcl,
                    Some(&schema),
                    Some(&name),
                    "table row type has an explicit ACL",
                ));
            }
            continue;
        }
        if is_generated_local_array {
            continue;
        }

        let kind = row.get::<_, String>("type_kind");
        return Err(refusal(
            PostgresIntrospectionErrorKind::UnsupportedCustomType,
            Some(&schema),
            Some(&name),
            format!("custom pg_type kind `{kind}` is outside the supported catalog set"),
        ));
    }
    Ok(())
}

async fn refuse_default_acls(
    client: &Client,
    schemas: &[String],
) -> Result<(), PostgresIntrospectionError> {
    let row = client
        .query_opt(DEFAULT_ACLS_SQL, &[&schemas])
        .await
        .map_err(|error| database_error("query configured-schema default ACLs", error))?;
    let Some(row) = row else {
        return Ok(());
    };
    let schema = row.get::<_, String>("schema_name");
    let owner = row.get::<_, String>("owner_name");
    let object_kind = row.get::<_, String>("object_kind");
    Err(refusal(
        PostgresIntrospectionErrorKind::UnsupportedAcl,
        Some(&schema),
        Some(&owner),
        format!("default ACL for object kind `{object_kind}` is not supported"),
    ))
}

async fn load_columns(
    client: &Client,
    schemas: &[String],
) -> Result<Vec<ColumnRow>, PostgresIntrospectionError> {
    client
        .query(COLUMNS_SQL, &[&schemas])
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
        default_collation: row.get("has_default_collation"),
        has_acl: row.get("has_acl"),
    }
}

fn map_columns(
    rows: &[ColumnRow],
    tables: &mut BTreeMap<TableKey, TableParts>,
) -> Result<BTreeMap<AttributeKey, String>, PostgresIntrospectionError> {
    let mut attributes = BTreeMap::new();
    for row in rows {
        let object = format!("{}.{}", row.table, row.name);
        if row.has_acl {
            return Err(refusal(
                PostgresIntrospectionErrorKind::UnsupportedAcl,
                Some(&row.schema),
                Some(&object),
                "column has an explicit ACL",
            ));
        }
        if !row.default_collation {
            return Err(refusal(
                PostgresIntrospectionErrorKind::UnsupportedColumnCollation,
                Some(&row.schema),
                Some(&object),
                "column must use its PostgreSQL type default collation",
            ));
        }

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
                        PostgresIntrospectionErrorKind::UnsupportedIdentity,
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
                        PostgresIntrospectionErrorKind::UnsupportedIdentity,
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
                        PostgresIntrospectionErrorKind::UnsupportedGeneratedColumn,
                        Some(&row.schema),
                        Some(&object),
                        "stored generated column has no pg_attrdef expression",
                    ));
                };
                (None, Some(ColumnGeneration::stored(expression.clone())))
            }
            ("", "v") => {
                return Err(refusal(
                    PostgresIntrospectionErrorKind::UnsupportedGeneratedColumn,
                    Some(&row.schema),
                    Some(&object),
                    "virtual generated columns are not supported",
                ));
            }
            (identity, generated) => {
                return Err(refusal(
                    PostgresIntrospectionErrorKind::UnsupportedGeneratedColumn,
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
                PostgresIntrospectionErrorKind::UnsupportedRelation,
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

async fn load_sequences(
    client: &Client,
    schemas: &[String],
) -> Result<Vec<SequenceRow>, PostgresIntrospectionError> {
    client
        .query(SEQUENCES_SQL, &[&schemas])
        .await
        .map_err(|error| database_error("query identity sequences", error))
        .map(|rows| rows.iter().map(sequence_row).collect())
}

fn sequence_row(row: &Row) -> SequenceRow {
    SequenceRow {
        schema: row.get("sequence_schema"),
        name: row.get("sequence_name"),
        dependency: row.get("dependency_kind"),
        table_schema: row.get("table_schema"),
        table: row.get("table_name"),
        column: row.get("column_name"),
        column_identity: row.get("column_identity"),
        type_name: row.get("type_name"),
        start: row.get("sequence_start"),
        increment: row.get("sequence_increment"),
        maximum: row.get("sequence_maximum"),
        minimum: row.get("sequence_minimum"),
        cache: row.get("sequence_cache"),
        cycle: row.get("sequence_cycle"),
    }
}

fn validate_sequences(
    rows: &[SequenceRow],
    columns: &[ColumnRow],
    schemas: &[String],
) -> Result<(), PostgresIntrospectionError> {
    let configured = schemas.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = columns
        .iter()
        .filter(|column| !column.identity.is_empty())
        .map(|column| {
            (
                column.schema.clone(),
                column.table.clone(),
                column.name.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();

    for row in rows {
        let Some(table_schema) = &row.table_schema else {
            return Err(refusal(
                PostgresIntrospectionErrorKind::UnsupportedSequence,
                Some(&row.schema),
                Some(&row.name),
                "standalone sequences are outside the supported catalog set",
            ));
        };
        let Some(table) = &row.table else {
            return Err(refusal(
                PostgresIntrospectionErrorKind::UnsupportedSequence,
                Some(&row.schema),
                Some(&row.name),
                "sequence dependency has no table",
            ));
        };
        let Some(column) = &row.column else {
            return Err(refusal(
                PostgresIntrospectionErrorKind::UnsupportedSequence,
                Some(&row.schema),
                Some(&row.name),
                "sequence dependency has no column",
            ));
        };
        let object = format!("{table}.{column}");
        if row.dependency.as_deref() != Some("i")
            || !matches!(row.column_identity.as_deref(), Some("a" | "d"))
        {
            return Err(refusal(
                PostgresIntrospectionErrorKind::UnsupportedSequence,
                Some(&row.schema),
                Some(&row.name),
                "only internally-owned identity sequences are supported",
            ));
        }
        if !configured.contains(table_schema.as_str()) || row.schema != *table_schema {
            return Err(refusal(
                PostgresIntrospectionErrorKind::UnsupportedIdentity,
                Some(table_schema),
                Some(&object),
                "identity sequence must share its configured table schema",
            ));
        }

        let default_maximum = match row.type_name.as_str() {
            "integer" => i64::from(i32::MAX),
            "bigint" => i64::MAX,
            unsupported => {
                return Err(refusal(
                    PostgresIntrospectionErrorKind::UnsupportedIdentity,
                    Some(table_schema),
                    Some(&object),
                    format!("unsupported identity sequence type `{unsupported}`"),
                ));
            }
        };
        if row.start != 1
            || row.increment != 1
            || row.minimum != 1
            || row.maximum != default_maximum
            || row.cache != 1
            || row.cycle
        {
            return Err(refusal(
                PostgresIntrospectionErrorKind::UnsupportedIdentity,
                Some(table_schema),
                Some(&object),
                "custom identity sequence options are not supported",
            ));
        }
        seen.insert((table_schema.clone(), table.clone(), column.clone()));
    }

    if let Some((schema, table, column)) = expected.difference(&seen).next() {
        return Err(refusal(
            PostgresIntrospectionErrorKind::UnsupportedIdentity,
            Some(schema),
            Some(&format!("{table}.{column}")),
            "identity column has no internally-owned sequence",
        ));
    }
    Ok(())
}

async fn load_constraints(
    client: &Client,
    schemas: &[String],
) -> Result<Vec<ConstraintRow>, PostgresIntrospectionError> {
    client
        .query(CONSTRAINTS_SQL, &[&schemas])
        .await
        .map_err(|error| database_error("query configured-schema constraints", error))
        .map(|rows| rows.iter().map(constraint_row).collect())
}

fn constraint_row(row: &Row) -> ConstraintRow {
    ConstraintRow {
        schema: row.get("schema_name"),
        table: row.get("table_name"),
        name: row.get("constraint_name"),
        kind: row.get("constraint_kind"),
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
    }
}

async fn load_indexes(
    client: &Client,
    schemas: &[String],
) -> Result<Vec<IndexRow>, PostgresIntrospectionError> {
    client
        .query(INDEXES_SQL, &[&schemas])
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
    kind: PostgresIntrospectionErrorKind,
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
                        kind,
                        Some(schema),
                        Some(object),
                        format!("catalog key references unsupported attribute number `{number}`"),
                    )
                })
        })
        .collect()
}

fn validate_authored_name(
    kind: PostgresIntrospectionErrorKind,
    schema: &str,
    table: &str,
    name: &str,
    suffix: &str,
    column_numbers: &[i16],
    attributes: &BTreeMap<AttributeKey, String>,
) -> Result<(), PostgresIntrospectionError> {
    if name.is_empty() {
        return Err(refusal(
            kind,
            Some(schema),
            Some(table),
            "catalog object name is empty",
        ));
    }

    let mut ordered_numbers = column_numbers.to_vec();
    ordered_numbers.sort_unstable();
    ordered_numbers.dedup();
    let columns = names_for_attributes(kind, schema, table, name, &ordered_numbers, attributes)?;
    let expected = if columns.is_empty() {
        format!("{table}_{suffix}")
    } else {
        format!("{table}_{}_{suffix}", columns.join("_"))
    };

    // PostgreSQL truncates identifiers at 63 bytes. The pre-apply validator
    // requires an explicit shorter name when the convention cannot fit, but
    // pg_catalog cannot reconstruct that authored spelling.
    if expected.len() < 64 && name != expected {
        return Err(refusal(
            kind,
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
        || (matches!(row.kind.as_str(), "c" | "n") && row.no_inherit)
        || row.period
        || row.parent
    {
        return Err(refusal(
            PostgresIntrospectionErrorKind::UnsupportedConstraint,
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
            PostgresIntrospectionErrorKind::UnsupportedConstraint,
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
            PostgresIntrospectionErrorKind::UnsupportedConstraint,
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
            PostgresIntrospectionErrorKind::UnsupportedConstraint,
            Some(&row.schema),
            Some(&row.name),
            format!("unsupported foreign-key action `{unsupported}`"),
        )),
    }
}

fn map_constraints(
    rows: &[ConstraintRow],
    attributes: &BTreeMap<AttributeKey, String>,
    tables: &mut BTreeMap<TableKey, TableParts>,
) -> Result<(), PostgresIntrospectionError> {
    for row in rows {
        validate_constraint_shape(row)?;
        if row.kind == "n" {
            continue;
        }

        let columns = names_for_attributes(
            PostgresIntrospectionErrorKind::UnsupportedConstraint,
            &row.schema,
            &row.table,
            &row.name,
            &row.columns,
            attributes,
        )?;
        let constraint = match row.kind.as_str() {
            "p" => {
                if columns.is_empty() {
                    return Err(refusal(
                        PostgresIntrospectionErrorKind::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "primary key has no columns",
                    ));
                }
                validate_authored_name(
                    PostgresIntrospectionErrorKind::UnsupportedConstraint,
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
                        PostgresIntrospectionErrorKind::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "unique constraint has no columns",
                    ));
                }
                validate_authored_name(
                    PostgresIntrospectionErrorKind::UnsupportedConstraint,
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
                        PostgresIntrospectionErrorKind::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "foreign key has missing or mismatched column pairs",
                    ));
                }
                if row.match_type != "s" || row.delete_column_subset {
                    return Err(refusal(
                        PostgresIntrospectionErrorKind::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "only MATCH SIMPLE foreign keys acting on every local column are supported",
                    ));
                }
                let Some(referenced_schema) = &row.referenced_schema else {
                    return Err(refusal(
                        PostgresIntrospectionErrorKind::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "foreign key has no referenced schema",
                    ));
                };
                let Some(referenced_table) = &row.referenced_table else {
                    return Err(refusal(
                        PostgresIntrospectionErrorKind::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "foreign key has no referenced table",
                    ));
                };
                if !tables.contains_key(&(referenced_schema.clone(), referenced_table.clone())) {
                    return Err(refusal(
                        PostgresIntrospectionErrorKind::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "foreign keys must reference an ordinary table in the configured schemas",
                    ));
                }
                let referenced_columns = names_for_attributes(
                    PostgresIntrospectionErrorKind::UnsupportedConstraint,
                    referenced_schema,
                    referenced_table,
                    &row.name,
                    &row.referenced_columns,
                    attributes,
                )?;
                validate_authored_name(
                    PostgresIntrospectionErrorKind::UnsupportedConstraint,
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
                        PostgresIntrospectionErrorKind::UnsupportedConstraint,
                        Some(&row.schema),
                        Some(&row.name),
                        "check constraint has no server expression",
                    ));
                };
                validate_authored_name(
                    PostgresIntrospectionErrorKind::UnsupportedConstraint,
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
                    PostgresIntrospectionErrorKind::UnsupportedConstraint,
                    Some(&row.schema),
                    Some(&row.name),
                    format!("unsupported pg_constraint contype `{unsupported}`"),
                ));
            }
        };

        let Some(table) = tables.get_mut(&(row.schema.clone(), row.table.clone())) else {
            return Err(refusal(
                PostgresIntrospectionErrorKind::UnsupportedConstraint,
                Some(&row.schema),
                Some(&row.name),
                "constraint belongs to a relation that is not an ordinary table",
            ));
        };
        table.constraints.push(constraint);
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
                PostgresIntrospectionErrorKind::UnsupportedIndex,
                Some(&row.schema),
                Some(&row.name),
                "only valid non-unique btree indexes over distinct named columns with default operator classes, collations, and null ordering are supported",
            ));
        }

        validate_authored_name(
            PostgresIntrospectionErrorKind::UnsupportedIndex,
            &row.schema,
            &row.table,
            &row.name,
            "idx",
            &row.column_numbers,
            attributes,
        )?;
        let names = names_for_attributes(
            PostgresIntrospectionErrorKind::UnsupportedIndex,
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
                        PostgresIntrospectionErrorKind::UnsupportedIndex,
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
                PostgresIntrospectionErrorKind::UnsupportedIndex,
                Some(&row.schema),
                Some(&row.name),
                error.to_string(),
            )
        })?;
        let Some(table) = tables.get_mut(&(row.schema.clone(), row.table.clone())) else {
            return Err(refusal(
                PostgresIntrospectionErrorKind::UnsupportedIndex,
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
    let schemas = configured_schemas(application_schemas);
    if schemas.is_empty() {
        return Ok(CatalogIr::new(Vec::new()));
    }

    validate_schemas(client, &schemas).await?;
    let relations = load_relations(client, &schemas).await?;
    let mut tables = validate_relations(&relations)?;
    refuse_routines(client, &schemas).await?;
    refuse_triggers(client, &schemas).await?;
    refuse_rules(client, &schemas).await?;
    refuse_policies(client, &schemas).await?;
    validate_types(client, &schemas).await?;
    refuse_default_acls(client, &schemas).await?;

    let columns = load_columns(client, &schemas).await?;
    let attributes = map_columns(&columns, &mut tables)?;
    let sequences = load_sequences(client, &schemas).await?;
    validate_sequences(&sequences, &columns, &schemas)?;

    let constraints = load_constraints(client, &schemas).await?;
    map_constraints(&constraints, &attributes, &mut tables)?;
    let indexes = load_indexes(client, &schemas).await?;
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
            })
            .collect(),
    ))
}
