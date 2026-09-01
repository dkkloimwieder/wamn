//! Normalized schema IR derived from PostgreSQL migrations.
//!
//! Frozen transport literals: `components/data/postgres-sqlx/wit/deps/wamn-postgres/package.wit`.
//! That contract returns the violated constraint or index name for SQLSTATE
//! 23505/23503/23514, so supported names are explicit authored IR, never
//! PostgreSQL-generated names.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A normalized application catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogIr {
    tables: Box<[Table]>,
}

impl CatalogIr {
    /// Normalize tables and every unordered collection below them.
    pub fn new(mut tables: Vec<Table>) -> Self {
        for table in &mut tables {
            table.normalize();
        }
        tables.sort();
        Self {
            tables: tables.into_boxed_slice(),
        }
    }

    /// Ordinary application tables, ordered by schema and table name.
    pub fn tables(&self) -> &[Table] {
        &self.tables
    }

    /// Serialize the complete normalized IR as stable compact JSON.
    pub fn canonical_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("the schema IR always serializes")
    }
}

/// One ordinary application table.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Table {
    schema: Box<str>,
    name: Box<str>,
    columns: Box<[Column]>,
    constraints: Box<[Constraint]>,
    indexes: Box<[Index]>,
}

impl Table {
    /// Construct one table; [`CatalogIr::new`] applies canonical ordering.
    pub fn new(
        schema: impl Into<Box<str>>,
        name: impl Into<Box<str>>,
        columns: Vec<Column>,
        constraints: Vec<Constraint>,
        indexes: Vec<Index>,
    ) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
            columns: columns.into_boxed_slice(),
            constraints: constraints.into_boxed_slice(),
            indexes: indexes.into_boxed_slice(),
        }
    }

    /// PostgreSQL schema name.
    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// Unqualified table name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Columns ordered by name.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// Constraints in canonical structural order.
    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// Ordinary indexes in canonical structural order.
    pub fn indexes(&self) -> &[Index] {
        &self.indexes
    }

    fn normalize(&mut self) {
        self.columns.sort();
        self.constraints.sort();
        self.indexes.sort();
    }
}

/// One supported table column.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Column {
    name: Box<str>,
    #[serde(rename = "type")]
    ty: ColumnType,
    nullable: bool,
    default: Option<ColumnDefault>,
    generation: Option<ColumnGeneration>,
}

impl Column {
    /// Construct one supported column.
    pub fn new(
        name: impl Into<Box<str>>,
        column_type: ColumnType,
        nullable: bool,
        default: Option<ColumnDefault>,
        generation: Option<ColumnGeneration>,
    ) -> Self {
        Self {
            name: name.into(),
            ty: column_type,
            nullable,
            default,
            generation,
        }
    }

    /// Column name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Frozen `wamn:postgres` transport type.
    pub const fn column_type(&self) -> ColumnType {
        self.ty
    }

    /// Whether SQL `NULL` is admitted for this column.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }

    /// Admitted semantic default, if any.
    pub const fn default(&self) -> Option<ColumnDefault> {
        self.default
    }

    /// Server-generated property, if any.
    pub fn generation(&self) -> Option<&ColumnGeneration> {
        self.generation.as_ref()
    }
}

/// The non-null cases of frozen `wamn:postgres/types.sql-value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
    Boolean,
    Int32,
    Int64,
    Float64,
    Text,
    Bytes,
    Numeric,
    Timestamptz,
    Json,
    Uuid,
}

impl ColumnType {
    /// Frozen wire literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Int32 => "int32",
            Self::Int64 => "int64",
            Self::Float64 => "float64",
            Self::Text => "text",
            Self::Bytes => "bytes",
            Self::Numeric => "numeric",
            Self::Timestamptz => "timestamptz",
            Self::Json => "json",
            Self::Uuid => "uuid",
        }
    }
}

/// Closed defaults demanded by the Receiving base and Acme overlay migrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnDefault {
    GenRandomUuid,
    CurrentTimestamp,
    TextOpen,
    TextNotRequired,
    TextPending,
    BooleanFalse,
    Int64One,
    NumericZero,
}

/// A PostgreSQL server-generated column property.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ColumnGeneration {
    Identity { mode: IdentityMode },
    Stored { expression: Box<str> },
}

impl ColumnGeneration {
    /// Construct a stored generated expression.
    pub fn stored(expression: impl Into<Box<str>>) -> Self {
        Self::Stored {
            expression: expression.into(),
        }
    }
}

/// PostgreSQL identity generation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityMode {
    Always,
    ByDefault,
}

/// A table constraint with its required authored name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Constraint {
    name: Box<str>,
    #[serde(flatten)]
    kind: ConstraintKind,
}

/// Semantic constraint shape, excluding its authored name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConstraintKind {
    PrimaryKey {
        columns: Box<[Box<str>]>,
    },
    Unique {
        columns: Box<[Box<str>]>,
    },
    ForeignKey {
        columns: Box<[ForeignKeyColumn]>,
        referenced_schema: Box<str>,
        referenced_table: Box<str>,
        on_update: ForeignKeyAction,
        on_delete: ForeignKeyAction,
    },
    Check {
        expression: Box<str>,
    },
}

impl Constraint {
    /// Construct a primary-key constraint.
    pub fn primary_key(
        name: impl Into<Box<str>>,
        columns: impl IntoIterator<Item = impl Into<Box<str>>>,
    ) -> Result<Self, IrError> {
        Self::new(
            name,
            "constraint",
            ConstraintKind::PrimaryKey {
                columns: boxed_strings(columns),
            },
        )
    }

    /// Construct a unique constraint.
    pub fn unique(
        name: impl Into<Box<str>>,
        columns: impl IntoIterator<Item = impl Into<Box<str>>>,
    ) -> Result<Self, IrError> {
        Self::new(
            name,
            "constraint",
            ConstraintKind::Unique {
                columns: boxed_strings(columns),
            },
        )
    }

    /// Construct a foreign-key constraint.
    pub fn foreign_key(
        name: impl Into<Box<str>>,
        columns: Vec<ForeignKeyColumn>,
        referenced_schema: impl Into<Box<str>>,
        referenced_table: impl Into<Box<str>>,
        on_update: ForeignKeyAction,
        on_delete: ForeignKeyAction,
    ) -> Result<Self, IrError> {
        Self::new(
            name,
            "constraint",
            ConstraintKind::ForeignKey {
                columns: columns.into_boxed_slice(),
                referenced_schema: referenced_schema.into(),
                referenced_table: referenced_table.into(),
                on_update,
                on_delete,
            },
        )
    }

    /// Construct a check constraint from its normalized expression.
    pub fn check(
        name: impl Into<Box<str>>,
        expression: impl Into<Box<str>>,
    ) -> Result<Self, IrError> {
        Self::new(
            name,
            "constraint",
            ConstraintKind::Check {
                expression: expression.into(),
            },
        )
    }

    /// Explicit authored name used by runtime violation errors.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Semantic constraint shape.
    pub const fn kind(&self) -> &ConstraintKind {
        &self.kind
    }

    fn new(
        name: impl Into<Box<str>>,
        object: &'static str,
        kind: ConstraintKind,
    ) -> Result<Self, IrError> {
        let name = nonempty_name(name, object)?;
        Ok(Self { name, kind })
    }
}

/// One local-to-referenced column pair in a foreign key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ForeignKeyColumn {
    column: Box<str>,
    referenced_column: Box<str>,
}

impl ForeignKeyColumn {
    /// Construct one foreign-key column pair.
    pub fn new(column: impl Into<Box<str>>, referenced_column: impl Into<Box<str>>) -> Self {
        Self {
            column: column.into(),
            referenced_column: referenced_column.into(),
        }
    }

    pub fn column(&self) -> &str {
        &self.column
    }

    pub fn referenced_column(&self) -> &str {
        &self.referenced_column
    }
}

/// PostgreSQL foreign-key action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForeignKeyAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

/// One ordinary, non-constraint-backed btree index.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Index {
    name: Box<str>,
    columns: Box<[IndexColumn]>,
}

impl Index {
    /// Construct an index. Key order is semantic and is preserved.
    pub fn new(name: impl Into<Box<str>>, columns: Vec<IndexColumn>) -> Result<Self, IrError> {
        Ok(Self {
            name: nonempty_name(name, "index")?,
            columns: columns.into_boxed_slice(),
        })
    }

    /// Explicit authored name used by runtime uniqueness errors.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn columns(&self) -> &[IndexColumn] {
        &self.columns
    }
}

/// One named column key in an ordinary index.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct IndexColumn {
    name: Box<str>,
    direction: IndexDirection,
}

impl IndexColumn {
    pub fn new(name: impl Into<Box<str>>, direction: IndexDirection) -> Self {
        Self {
            name: name.into(),
            direction,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn direction(&self) -> IndexDirection {
        self.direction
    }
}

/// Key direction for an ordinary btree index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexDirection {
    Asc,
    Desc,
}

/// Stable class of IR construction refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrErrorKind {
    EmptyName,
    UnsupportedType,
    UnsupportedDefault,
}

/// Typed IR construction refusal with the offending PostgreSQL input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrError {
    kind: IrErrorKind,
    input: Box<str>,
    column_type: Option<ColumnType>,
}

impl IrError {
    /// Stable refusal class.
    pub const fn kind(&self) -> IrErrorKind {
        self.kind
    }

    /// PostgreSQL input, or the object class whose name was empty.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Column type context for a default refusal.
    pub const fn column_type(&self) -> Option<ColumnType> {
        self.column_type
    }
}

impl fmt::Display for IrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            IrErrorKind::EmptyName => write!(formatter, "{} name must not be empty", self.input),
            IrErrorKind::UnsupportedType => {
                write!(formatter, "unsupported PostgreSQL type `{}`", self.input)
            }
            IrErrorKind::UnsupportedDefault => write!(
                formatter,
                "unsupported PostgreSQL default `{}` for wamn:postgres type `{}`",
                self.input,
                self.column_type
                    .expect("default refusals always carry type context")
                    .as_str()
            ),
        }
    }
}

impl std::error::Error for IrError {}

/// Map a PostgreSQL catalog type spelling to the frozen transport vocabulary.
pub fn postgres_type(postgres_name: &str) -> Result<ColumnType, IrError> {
    match postgres_name {
        "boolean" => Ok(ColumnType::Boolean),
        "integer" => Ok(ColumnType::Int32),
        "bigint" => Ok(ColumnType::Int64),
        "double precision" => Ok(ColumnType::Float64),
        "text" => Ok(ColumnType::Text),
        "bytea" => Ok(ColumnType::Bytes),
        "numeric" => Ok(ColumnType::Numeric),
        "timestamp with time zone" => Ok(ColumnType::Timestamptz),
        "jsonb" => Ok(ColumnType::Json),
        "uuid" => Ok(ColumnType::Uuid),
        unsupported => Err(IrError {
            kind: IrErrorKind::UnsupportedType,
            input: unsupported.into(),
            column_type: None,
        }),
    }
}

/// Normalize an admitted `pg_get_expr` default to its semantic IR variant.
pub fn postgres_default(
    column_type: ColumnType,
    expression: &str,
) -> Result<ColumnDefault, IrError> {
    let normalized = expression.trim();
    let default = match (column_type, normalized) {
        (ColumnType::Uuid, "gen_random_uuid()") => ColumnDefault::GenRandomUuid,
        (ColumnType::Timestamptz, "CURRENT_TIMESTAMP") => ColumnDefault::CurrentTimestamp,
        (ColumnType::Text, "'open'::text" | "'open'") => ColumnDefault::TextOpen,
        (ColumnType::Text, "'not_required'::text" | "'not_required'") => {
            ColumnDefault::TextNotRequired
        }
        (ColumnType::Text, "'pending'::text" | "'pending'") => ColumnDefault::TextPending,
        (ColumnType::Boolean, "false" | "'false'::boolean") => ColumnDefault::BooleanFalse,
        (ColumnType::Int64, "1" | "'1'::bigint") => ColumnDefault::Int64One,
        (ColumnType::Numeric, "0" | "'0'::numeric" | "0::numeric") => ColumnDefault::NumericZero,
        _ => {
            return Err(IrError {
                kind: IrErrorKind::UnsupportedDefault,
                input: normalized.into(),
                column_type: Some(column_type),
            });
        }
    };
    Ok(default)
}

fn boxed_strings(values: impl IntoIterator<Item = impl Into<Box<str>>>) -> Box<[Box<str>]> {
    values
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn nonempty_name(name: impl Into<Box<str>>, object: &'static str) -> Result<Box<str>, IrError> {
    let name = name.into();
    if name.is_empty() {
        return Err(IrError {
            kind: IrErrorKind::EmptyName,
            input: object.into(),
            column_type: None,
        });
    }
    Ok(name)
}
