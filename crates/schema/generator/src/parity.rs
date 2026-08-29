//! Validates the generated native/Wamn PostgreSQL projection parity matrix.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::Deserialize;

const PARITY_RULE: &str = "same_sql_file_two_projection_structs";

/// Stable classification of parity-matrix refusals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParityErrorKind {
    /// The bytes are not a strict parity-matrix JSON document.
    InvalidJson,
    /// The matrix is structurally incomplete or internally ambiguous.
    InvalidMatrix,
    /// A PostgreSQL type is outside the frozen supported vocabulary.
    UnsupportedPostgresType,
    /// A declared carrier does not match the frozen PostgreSQL mapping.
    TypeMismatch,
}

/// A fail-closed parity refusal with model-field context when available.
#[derive(Debug)]
pub struct ParityError {
    kind: ParityErrorKind,
    context: Box<str>,
    field: Option<Box<str>>,
    source: Option<serde_json::Error>,
}

impl ParityError {
    /// Stable refusal class for callers that must not match display text.
    pub const fn kind(&self) -> ParityErrorKind {
        self.kind
    }

    /// Human-readable reason for the refusal.
    pub fn context(&self) -> &str {
        &self.context
    }

    /// Qualified field or accessor parameter that failed validation.
    pub fn field(&self) -> Option<&str> {
        self.field.as_deref()
    }

    fn matrix(kind: ParityErrorKind, context: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            context: context.into(),
            field: None,
            source: None,
        }
    }

    fn for_field(
        kind: ParityErrorKind,
        field: impl Into<Box<str>>,
        context: impl Into<Box<str>>,
    ) -> Self {
        Self {
            kind,
            context: context.into(),
            field: Some(field.into()),
            source: None,
        }
    }
}

impl From<serde_json::Error> for ParityError {
    fn from(source: serde_json::Error) -> Self {
        Self {
            kind: ParityErrorKind::InvalidJson,
            context: "parity JSON does not match the closed matrix vocabulary".into(),
            field: None,
            source: Some(source),
        }
    }
}

impl fmt::Display for ParityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(field) = &self.field {
            write!(formatter, "{:?}: {field}: {}", self.kind, self.context)
        } else {
            write!(formatter, "{:?}: {}", self.kind, self.context)
        }
    }
}

impl Error for ParityError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParityMatrix {
    model: String,
    rule: String,
    fields: Vec<ParityField>,
    accessor_binds: Vec<ParityAccessorBind>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParityField {
    field: String,
    postgres: String,
    wamn_sql_value: String,
    native_rust: String,
    wamn_rust: String,
    nullable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParityAccessorBind {
    accessor: String,
    parameter: String,
    postgres: String,
    nullable: bool,
    native_rust: String,
    wamn_rust: String,
}

#[derive(Debug, Clone, Copy)]
struct TypeMapping {
    wamn_sql_value: &'static str,
    native_rust: &'static str,
    wamn_rust: &'static str,
}

/// Validate one generated parity JSON artifact against the frozen type seam.
pub fn validate_parity_json(bytes: &[u8]) -> Result<(), ParityError> {
    let matrix: ParityMatrix = serde_json::from_slice(bytes)?;
    if matrix.model.is_empty() {
        return Err(ParityError::matrix(
            ParityErrorKind::InvalidMatrix,
            "model must not be empty",
        ));
    }
    if matrix.rule != PARITY_RULE {
        return Err(ParityError::matrix(
            ParityErrorKind::InvalidMatrix,
            format!("rule must be `{PARITY_RULE}`"),
        ));
    }
    if matrix.fields.is_empty() {
        return Err(ParityError::matrix(
            ParityErrorKind::InvalidMatrix,
            format!("{} must declare at least one field", matrix.model),
        ));
    }

    let mut names = BTreeSet::new();
    for field in &matrix.fields {
        validate_field(&matrix.model, field, &mut names)?;
    }
    if matrix.accessor_binds.is_empty() {
        return Err(ParityError::matrix(
            ParityErrorKind::InvalidMatrix,
            format!("{} must declare at least one accessor bind", matrix.model),
        ));
    }
    let mut identities = BTreeSet::new();
    for bind in &matrix.accessor_binds {
        validate_accessor_bind(&matrix.model, bind, &mut identities)?;
    }
    Ok(())
}

fn validate_accessor_bind<'a>(
    model: &str,
    bind: &'a ParityAccessorBind,
    identities: &mut BTreeSet<(&'a str, &'a str)>,
) -> Result<(), ParityError> {
    let identity = format!("{model}.{}({})", bind.accessor, bind.parameter);
    if bind.accessor.is_empty() || bind.parameter.is_empty() {
        return Err(ParityError::for_field(
            ParityErrorKind::InvalidMatrix,
            identity,
            "accessor and parameter identities must not be empty",
        ));
    }
    if !identities.insert((bind.accessor.as_str(), bind.parameter.as_str())) {
        return Err(ParityError::for_field(
            ParityErrorKind::InvalidMatrix,
            identity,
            "accessor parameter is repeated",
        ));
    }
    let mapping = type_mapping(&bind.postgres).ok_or_else(|| {
        ParityError::for_field(
            ParityErrorKind::UnsupportedPostgresType,
            identity.clone(),
            format!(
                "PostgreSQL type `{}` is outside the frozen parity vocabulary",
                bind.postgres
            ),
        )
    })?;
    require_exact(
        &identity,
        "native_rust",
        &bind.postgres,
        &projected_type(mapping.native_rust, bind.nullable),
        &bind.native_rust,
        bind.nullable,
    )?;
    require_exact(
        &identity,
        "wamn_rust",
        &bind.postgres,
        &projected_type(mapping.wamn_rust, bind.nullable),
        &bind.wamn_rust,
        bind.nullable,
    )
}

fn validate_field<'a>(
    model: &str,
    field: &'a ParityField,
    names: &mut BTreeSet<&'a str>,
) -> Result<(), ParityError> {
    if field.field.is_empty() {
        return Err(ParityError::matrix(
            ParityErrorKind::InvalidMatrix,
            format!("{model} contains an empty field name"),
        ));
    }
    let qualified = format!("{model}.{}", field.field);
    if !names.insert(&field.field) {
        return Err(ParityError::for_field(
            ParityErrorKind::InvalidMatrix,
            qualified,
            "field is repeated",
        ));
    }

    let mapping = type_mapping(&field.postgres).ok_or_else(|| {
        ParityError::for_field(
            ParityErrorKind::UnsupportedPostgresType,
            qualified.clone(),
            format!(
                "PostgreSQL type `{}` is outside the frozen parity vocabulary",
                field.postgres
            ),
        )
    })?;

    require_exact(
        &qualified,
        "wamn_sql_value",
        &field.postgres,
        mapping.wamn_sql_value,
        &field.wamn_sql_value,
        field.nullable,
    )?;
    require_exact(
        &qualified,
        "native_rust",
        &field.postgres,
        &projected_type(mapping.native_rust, field.nullable),
        &field.native_rust,
        field.nullable,
    )?;
    require_exact(
        &qualified,
        "wamn_rust",
        &field.postgres,
        &projected_type(mapping.wamn_rust, field.nullable),
        &field.wamn_rust,
        field.nullable,
    )
}

fn require_exact(
    field: &str,
    member: &str,
    postgres: &str,
    expected: &str,
    actual: &str,
    nullable: bool,
) -> Result<(), ParityError> {
    if actual == expected {
        return Ok(());
    }
    Err(ParityError::for_field(
        ParityErrorKind::TypeMismatch,
        field,
        format!(
            "{member} for PostgreSQL `{postgres}` with nullable={nullable} must be `{expected}`, found `{actual}`"
        ),
    ))
}

fn projected_type(base: &str, nullable: bool) -> String {
    if nullable {
        format!("Option<{base}>")
    } else {
        base.to_owned()
    }
}

fn type_mapping(postgres: &str) -> Option<TypeMapping> {
    let mapping = match postgres {
        "boolean" => TypeMapping {
            wamn_sql_value: "boolean",
            native_rust: "bool",
            wamn_rust: "bool",
        },
        "int4" => TypeMapping {
            wamn_sql_value: "int32",
            native_rust: "i32",
            wamn_rust: "i32",
        },
        "int8" => TypeMapping {
            wamn_sql_value: "int64",
            native_rust: "i64",
            wamn_rust: "i64",
        },
        "float8" => TypeMapping {
            wamn_sql_value: "float64",
            native_rust: "f64",
            wamn_rust: "f64",
        },
        "text" => TypeMapping {
            wamn_sql_value: "text",
            native_rust: "String",
            wamn_rust: "String",
        },
        "bytea" => TypeMapping {
            wamn_sql_value: "bytes",
            native_rust: "Vec<u8>",
            wamn_rust: "Vec<u8>",
        },
        "numeric" => TypeMapping {
            wamn_sql_value: "numeric",
            native_rust: "rust_decimal::Decimal",
            wamn_rust: "wamn_postgres_sqlx::Numeric",
        },
        "timestamptz" => TypeMapping {
            wamn_sql_value: "timestamptz",
            native_rust: "chrono::DateTime<chrono::Utc>",
            wamn_rust: "wamn_postgres_sqlx::TimestampTz",
        },
        "jsonb" => TypeMapping {
            wamn_sql_value: "json",
            native_rust: "serde_json::Value",
            wamn_rust: "wamn_postgres_sqlx::Json",
        },
        "uuid" => TypeMapping {
            wamn_sql_value: "uuid",
            native_rust: "uuid::Uuid",
            wamn_rust: "wamn_postgres_sqlx::Uuid",
        },
        _ => return None,
    };
    Some(mapping)
}
