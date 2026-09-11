//! Normalized schema IR derived from PostgreSQL migrations.
//!
//! Frozen transport literals: `apps/platform/data/postgres-sqlx/wit/deps/wamn-postgres/package.wit`.
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
    /// Omitted when empty, so a table that declares no exclusion constraint
    /// keeps the canonical bytes it had before this field existed.
    #[serde(skip_serializing_if = "<[Exclusion]>::is_empty")]
    exclusions: Box<[Exclusion]>,
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
            exclusions: Box::default(),
        }
    }

    /// Attach this table's exclusion constraints.
    #[must_use]
    pub fn with_exclusions(mut self, exclusions: Vec<Exclusion>) -> Self {
        self.exclusions = exclusions.into_boxed_slice();
        self
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

    /// Exclusion constraints in canonical structural order.
    pub fn exclusions(&self) -> &[Exclusion] {
        &self.exclusions
    }

    fn normalize(&mut self) {
        self.columns.sort();
        self.constraints.sort();
        self.indexes.sort();
        self.exclusions.sort();
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
    ///
    /// Borrowed rather than copied: a default now carries its VALUE, so the
    /// enum is no longer `Copy` (`wamn-frru`).
    pub const fn default(&self) -> Option<&ColumnDefault> {
        self.default.as_ref()
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

/// One admitted column default: a named server function, or a literal of the
/// column's own type carrying its VALUE.
///
/// The variants were once Receiving's own words, `TextOpen`, `TextNotRequired`
/// and `TextPending`, which put one package's vocabulary in the platform and
/// refused every other author's word. Three independent agents wrote
/// `DEFAULT 'scheduled'` for a status column and all three were refused at
/// Introspect (`wamn-frru`). A default's FORM is platform vocabulary. Its VALUE
/// is the package's own, and R-A already rules that package-local values expand
/// under the validator.
///
/// The allowlist therefore closes over forms. What it still refuses is
/// unchanged in kind: an expression, a function call other than the two named
/// here, and a literal whose type is not the column's.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ColumnDefault {
    GenRandomUuid,
    CurrentTimestamp,
    Text {
        value: Box<str>,
    },
    Boolean {
        value: bool,
    },
    Int64 {
        value: i64,
    },
    /// Carried as text, because a numeric default keeps the precision the
    /// author wrote and `f64` would not.
    Numeric {
        value: Box<str>,
    },
}

impl ColumnDefault {
    /// A text literal default.
    pub fn text(value: impl Into<Box<str>>) -> Self {
        Self::Text {
            value: value.into(),
        }
    }

    /// A boolean literal default.
    pub const fn boolean(value: bool) -> Self {
        Self::Boolean { value }
    }

    /// A bigint literal default.
    pub const fn int64(value: i64) -> Self {
        Self::Int64 { value }
    }

    /// A numeric literal default, carried verbatim.
    pub fn numeric(value: impl Into<Box<str>>) -> Self {
        Self::Numeric {
            value: value.into(),
        }
    }
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

/// One `EXCLUDE` constraint with its required authored name.
///
/// An exclusion constraint is carried beside [`Constraint`] rather than inside
/// it because its keys are operator comparisons over a column OR an expression,
/// which a [`ConstraintKind`]'s plain column list cannot hold (wamn-10yt.36).
/// Both are nameable refusals: the `wamn:postgres` contract carries SQLSTATE
/// 23505, 23503 and 23514 as [`Constraint`] violations and 23P01 as an
/// exclusion violation, each naming the constraint that refused (wamn-10yt.54).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Exclusion {
    name: Box<str>,
    access_method: ExclusionAccessMethod,
    keys: Box<[ExclusionKey]>,
    /// Every column the constraint depends on, including the columns an
    /// expression key reads, sorted and deduplicated.
    ///
    /// PostgreSQL records these as auto dependencies of the constraint and of
    /// its index -- the same dependency that blocks `DROP COLUMN` -- so the set
    /// is read from the catalog and never parsed out of the expression text.
    columns: Box<[Box<str>]>,
}

impl Exclusion {
    /// Construct one exclusion constraint. Key order is semantic and preserved;
    /// the dependency columns are a set, so they are sorted and deduplicated.
    pub fn new(
        name: impl Into<Box<str>>,
        access_method: ExclusionAccessMethod,
        keys: Vec<ExclusionKey>,
        columns: impl IntoIterator<Item = impl Into<Box<str>>>,
    ) -> Result<Self, IrError> {
        let mut columns = boxed_strings(columns).into_vec();
        columns.sort();
        columns.dedup();
        Ok(Self {
            name: nonempty_name(name, "exclusion constraint")?,
            access_method,
            keys: keys.into_boxed_slice(),
            columns: columns.into_boxed_slice(),
        })
    }

    /// Explicit authored name PostgreSQL reports on a 23P01 violation.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Index access method resolving the key operators.
    pub const fn access_method(&self) -> ExclusionAccessMethod {
        self.access_method
    }

    /// Compared keys in the order the constraint declares them.
    pub fn keys(&self) -> &[ExclusionKey] {
        &self.keys
    }

    /// Every column a write must touch to be capable of violating this
    /// constraint, expression-referenced columns included.
    pub fn columns(&self) -> &[Box<str>] {
        &self.columns
    }
}

/// Index access method backing an exclusion constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionAccessMethod {
    Gist,
}

/// One key of an exclusion constraint: what is compared, and with which operator.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ExclusionKey {
    #[serde(flatten)]
    element: ExclusionElement,
    operator: Box<str>,
}

impl ExclusionKey {
    /// Construct one compared key.
    pub fn new(element: ExclusionElement, operator: impl Into<Box<str>>) -> Self {
        Self {
            element,
            operator: operator.into(),
        }
    }

    /// Compared side of this key.
    pub const fn element(&self) -> &ExclusionElement {
        &self.element
    }

    /// Bare PostgreSQL operator name, such as `=` or `&&`.
    pub fn operator(&self) -> &str {
        &self.operator
    }
}

/// The compared side of one exclusion key.
///
/// A range non-overlap invariant needs an expression key, because the frozen
/// column vocabulary has no range type: the author writes
/// `tstzrange(starts_at, ends_at) WITH &&` over two `timestamptz` columns.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "element", rename_all = "snake_case")]
pub enum ExclusionElement {
    Column { name: Box<str> },
    Expression { expression: Box<str> },
}

impl ExclusionElement {
    /// A plain column key.
    pub fn column(name: impl Into<Box<str>>) -> Self {
        Self::Column { name: name.into() }
    }

    /// An expression key, carried as the server renders it.
    pub fn expression(expression: impl Into<Box<str>>) -> Self {
        Self::Expression {
            expression: expression.into(),
        }
    }
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
///
/// Two server functions are admitted by name, each for the one type it serves.
/// Everything else must be a LITERAL of the column's own type, optionally cast
/// to that same type. An expression, any other function call, and a literal of
/// a different type all refuse, which is the whole wall this allowlist keeps.
pub fn postgres_default(
    column_type: ColumnType,
    expression: &str,
) -> Result<ColumnDefault, IrError> {
    let normalized = expression.trim();
    let refuse = || IrError {
        kind: IrErrorKind::UnsupportedDefault,
        input: normalized.into(),
        column_type: Some(column_type),
    };

    match (column_type, normalized) {
        (ColumnType::Uuid, "gen_random_uuid()") => return Ok(ColumnDefault::GenRandomUuid),
        (ColumnType::Timestamptz, "CURRENT_TIMESTAMP") => {
            return Ok(ColumnDefault::CurrentTimestamp);
        }
        _ => {}
    }

    let literal = strip_own_cast(normalized, column_type).ok_or_else(refuse)?;
    match column_type {
        ColumnType::Text => quoted_value(literal)
            .map(ColumnDefault::text)
            .ok_or_else(refuse),
        ColumnType::Boolean => match unquoted(literal).to_ascii_lowercase().as_str() {
            "true" => Ok(ColumnDefault::boolean(true)),
            "false" => Ok(ColumnDefault::boolean(false)),
            _ => Err(refuse()),
        },
        ColumnType::Int64 => unquoted(literal)
            .parse::<i64>()
            .map(ColumnDefault::int64)
            .map_err(|_| refuse()),
        ColumnType::Numeric => {
            let value = unquoted(literal);
            if is_numeric_literal(&value) {
                Ok(ColumnDefault::numeric(value))
            } else {
                Err(refuse())
            }
        }
        // Int32, Float64, Bytes, Json and Uuid admit no literal default form
        // yet. Adding one is the same shape as this function's other arms.
        _ => Err(refuse()),
    }
}

/// Remove a trailing `::type` cast when it names the column's OWN type.
///
/// A cast to any other type is a type mismatch, not a default, so it refuses by
/// returning `None` rather than by silently dropping the cast.
fn strip_own_cast(expression: &str, column_type: ColumnType) -> Option<&str> {
    let Some((value, cast)) = expression.rsplit_once("::") else {
        return Some(expression.trim());
    };
    let cast = cast.trim();
    if postgres_type(cast).ok()? == column_type {
        Some(value.trim())
    } else {
        None
    }
}

/// The inside of a single-quoted SQL string, with doubled quotes collapsed.
fn quoted_value(literal: &str) -> Option<Box<str>> {
    let inner = literal.strip_prefix('\'')?.strip_suffix('\'')?;
    // A lone quote inside would have ended the literal, so any quote left here
    // must be one of a doubled pair.
    if inner.replace("''", "").contains('\'') {
        return None;
    }
    Some(inner.replace("''", "'").into())
}

/// A quoted literal's contents, or the bare token when it carries no quotes.
fn unquoted(literal: &str) -> String {
    quoted_value(literal).map_or_else(|| literal.to_owned(), str::into_string)
}

/// A decimal numeric literal, with an optional sign and one optional point.
fn is_numeric_literal(value: &str) -> bool {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    let mut parts = digits.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next().unwrap_or("0");
    parts.next().is_none()
        && !whole.is_empty()
        && !fraction.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
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
