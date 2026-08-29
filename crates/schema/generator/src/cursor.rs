use std::error::Error;
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, SecondsFormat};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wamn_schema_introspection::ir::ColumnType;

use crate::CursorDirection;

const VERSION: u8 = 1;

/// A cursor key whose JSON representation follows its migration-derived type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorValue {
    Boolean(bool),
    Int32(i32),
    Int64(i64),
    Text(Box<str>),
    Numeric(Box<str>),
    Timestamptz(Box<str>),
    Uuid(Box<str>),
}

impl CursorValue {
    /// Frozen catalog type represented by this cursor value.
    pub const fn column_type(&self) -> ColumnType {
        match self {
            Self::Boolean(_) => ColumnType::Boolean,
            Self::Int32(_) => ColumnType::Int32,
            Self::Int64(_) => ColumnType::Int64,
            Self::Text(_) => ColumnType::Text,
            Self::Numeric(_) => ColumnType::Numeric,
            Self::Timestamptz(_) => ColumnType::Timestamptz,
            Self::Uuid(_) => ColumnType::Uuid,
        }
    }

    fn as_json(&self) -> Value {
        match self {
            Self::Boolean(value) => Value::Bool(*value),
            Self::Int32(value) => Value::from(*value),
            Self::Int64(value) => Value::from(*value),
            Self::Text(value)
            | Self::Numeric(value)
            | Self::Timestamptz(value)
            | Self::Uuid(value) => Value::String(value.to_string()),
        }
    }
}

/// Decoded version-one keyset cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorV1 {
    field: Box<str>,
    direction: CursorDirection,
    key: CursorValue,
    id: Box<str>,
}

impl CursorV1 {
    /// Construct a cursor; [`encode_cursor`] validates canonical typed values.
    pub fn new(
        field: impl Into<Box<str>>,
        direction: CursorDirection,
        key: CursorValue,
        id: impl Into<Box<str>>,
    ) -> Self {
        Self {
            field: field.into(),
            direction,
            key,
            id: id.into(),
        }
    }

    /// Declared sort field.
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Sort direction inherited by the `id` tie-breaker.
    pub const fn direction(&self) -> CursorDirection {
        self.direction
    }

    /// Typed PostgreSQL key spelling.
    pub const fn key(&self) -> &CursorValue {
        &self.key
    }

    /// Canonical UUID tie-breaker.
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Stable cursor refusal class exposed to operation input translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorErrorKind {
    InvalidInput,
}

/// Cursor refusal translated to the frozen `invalid_input` operation literal.
#[derive(Debug)]
pub struct CursorError {
    kind: CursorErrorKind,
    context: Box<str>,
    source: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl CursorError {
    /// Stable refusal class; malformed cursors never become first-page requests.
    pub const fn kind(&self) -> CursorErrorKind {
        self.kind
    }

    /// Non-wire diagnostic for the package implementation.
    pub fn context(&self) -> &str {
        &self.context
    }

    fn invalid(context: impl Into<Box<str>>) -> Self {
        Self {
            kind: CursorErrorKind::InvalidInput,
            context: context.into(),
            source: None,
        }
    }

    fn invalid_source(
        context: impl Into<Box<str>>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind: CursorErrorKind::InvalidInput,
            context: context.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for CursorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid_input: {}", self.context)
    }
}

impl Error for CursorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[derive(Debug, Serialize)]
struct CursorWire<'a> {
    v: u8,
    field: &'a str,
    direction: CursorDirection,
    key: Value,
    id: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecodedCursorWire {
    v: u8,
    field: Box<str>,
    direction: CursorDirection,
    key: Value,
    id: Box<str>,
}

/// Encode exact canonical compact JSON as unpadded base64url.
pub fn encode_cursor(cursor: &CursorV1) -> Result<String, CursorError> {
    validate_field(&cursor.field)?;
    validate_uuid(&cursor.id, "cursor id")?;
    validate_value(&cursor.key)?;
    let bytes = serde_json::to_vec(&CursorWire {
        v: VERSION,
        field: &cursor.field,
        direction: cursor.direction,
        key: cursor.key.as_json(),
        id: &cursor.id,
    })
    .expect("cursor wire values always serialize");
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Decode and validate one cursor for an exact manifest field and direction.
pub fn decode_cursor(
    encoded: &str,
    expected_field: &str,
    expected_direction: CursorDirection,
    expected_type: ColumnType,
) -> Result<CursorV1, CursorError> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|source| {
        CursorError::invalid_source("cursor is not unpadded base64url", source)
    })?;
    let wire: DecodedCursorWire = serde_json::from_slice(&bytes).map_err(|source| {
        CursorError::invalid_source("cursor payload is not the closed v1 JSON shape", source)
    })?;
    if wire.v != VERSION {
        return Err(CursorError::invalid("cursor version is not supported"));
    }
    if wire.field.as_ref() != expected_field || wire.direction != expected_direction {
        return Err(CursorError::invalid(
            "cursor field or direction does not match the requested sort",
        ));
    }
    validate_field(&wire.field)?;
    validate_uuid(&wire.id, "cursor id")?;
    let key = value_from_json(expected_type, wire.key)?;
    let cursor = CursorV1 {
        field: wire.field,
        direction: wire.direction,
        key,
        id: wire.id,
    };
    let canonical = canonical_bytes(&cursor);
    if canonical != bytes || URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return Err(CursorError::invalid("cursor is not canonically encoded"));
    }
    Ok(cursor)
}

fn canonical_bytes(cursor: &CursorV1) -> Vec<u8> {
    serde_json::to_vec(&CursorWire {
        v: VERSION,
        field: &cursor.field,
        direction: cursor.direction,
        key: cursor.key.as_json(),
        id: &cursor.id,
    })
    .expect("cursor wire values always serialize")
}

fn value_from_json(expected: ColumnType, value: Value) -> Result<CursorValue, CursorError> {
    let cursor_value = match (expected, value) {
        (ColumnType::Boolean, Value::Bool(value)) => CursorValue::Boolean(value),
        (ColumnType::Int32, Value::Number(value)) => value
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .map(CursorValue::Int32)
            .ok_or_else(|| CursorError::invalid("cursor key is not an int32"))?,
        (ColumnType::Int64, Value::Number(value)) => value
            .as_i64()
            .map(CursorValue::Int64)
            .ok_or_else(|| CursorError::invalid("cursor key is not an int64"))?,
        (ColumnType::Text, Value::String(value)) => CursorValue::Text(value.into_boxed_str()),
        (ColumnType::Numeric, Value::String(value)) => CursorValue::Numeric(value.into_boxed_str()),
        (ColumnType::Timestamptz, Value::String(value)) => {
            CursorValue::Timestamptz(value.into_boxed_str())
        }
        (ColumnType::Uuid, Value::String(value)) => CursorValue::Uuid(value.into_boxed_str()),
        (ColumnType::Float64 | ColumnType::Bytes | ColumnType::Json, _) => {
            return Err(CursorError::invalid(
                "cursor field type is outside the closed sortable vocabulary",
            ));
        }
        _ => {
            return Err(CursorError::invalid(
                "cursor key type does not match the sort field",
            ));
        }
    };
    validate_value(&cursor_value)?;
    Ok(cursor_value)
}

fn validate_value(value: &CursorValue) -> Result<(), CursorError> {
    match value {
        CursorValue::Numeric(value) if !canonical_numeric(value) => Err(CursorError::invalid(
            "numeric cursor key is not a canonical PostgreSQL lexical value",
        )),
        CursorValue::Timestamptz(value) if !canonical_timestamptz(value) => Err(
            CursorError::invalid("timestamptz cursor key must be UTC RFC3339 with six digits"),
        ),
        CursorValue::Uuid(value) => validate_uuid(value, "cursor key"),
        _ => Ok(()),
    }
}

fn validate_field(field: &str) -> Result<(), CursorError> {
    let mut bytes = field.bytes();
    if bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && !field.ends_with('_')
        && !field.contains("__")
    {
        Ok(())
    } else {
        Err(CursorError::invalid("cursor field is not snake_case"))
    }
}

fn canonical_numeric(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let start = usize::from(bytes[0] == b'-');
    if start == bytes.len() {
        return false;
    }
    let rest = &bytes[start..];
    let integer_end = rest
        .iter()
        .position(|byte| *byte == b'.')
        .unwrap_or(rest.len());
    let integer = &rest[..integer_end];
    if integer.is_empty()
        || !integer.iter().all(u8::is_ascii_digit)
        || (integer.len() > 1 && integer[0] == b'0')
    {
        return false;
    }
    if integer_end == rest.len() {
        return true;
    }
    let fraction = &rest[integer_end + 1..];
    !fraction.is_empty() && fraction.iter().all(u8::is_ascii_digit)
}

fn canonical_timestamptz(value: &str) -> bool {
    DateTime::parse_from_rfc3339(value).is_ok_and(|timestamp| {
        timestamp
            .to_utc()
            .to_rfc3339_opts(SecondsFormat::Micros, true)
            == value
    })
}

fn validate_uuid(value: &str, object: &str) -> Result<(), CursorError> {
    let bytes = value.as_bytes();
    let valid = bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
            }
        });
    if valid {
        Ok(())
    } else {
        Err(CursorError::invalid(format!(
            "{object} is not a canonical lowercase UUID"
        )))
    }
}
