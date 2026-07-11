//! Transport-agnostic SQL value.
//!
//! [`SqlValue`] is a 1:1 mirror of the frozen `wamn:postgres@0.1.0` WIT
//! `sql-value` variant (same case names, same payloads). The serving component
//! maps between this and the wit-bindgen-generated `SqlValue` with a trivial
//! match, which keeps the gateway *logic* free of any Wasm binding — the crate
//! is pure Rust, unit-testable with no host and no database.
//!
//! Numeric / timestamptz / uuid / json travel as **canonical strings**. There
//! is deliberately no float path for catalog data: the 3.1 no-float rule holds
//! end to end, so a `numeric` column is a decimal string in both a bound
//! parameter and the shaped response (never a lossy JSON number).

use serde_json::Value;

/// A single bound parameter or result cell. Variants match the `wamn:postgres`
/// `sql-value` cases exactly.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Bool(bool),
    Int32(i32),
    Int64(i64),
    /// Present only to mirror the WIT; the gateway never *produces* a float
    /// (catalog data has no float type), but a row-set cell could carry one.
    Float64(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// Exact decimal as a canonical string, e.g. `"12.50"`.
    Numeric(String),
    /// RFC 3339 timestamp string.
    Timestamptz(String),
    /// A JSON document string (a `jsonb` column).
    Json(String),
    /// Canonical UUID string.
    Uuid(String),
}

impl SqlValue {
    /// Shape a result cell into JSON for the response body.
    ///
    /// - `numeric` becomes a JSON **string** (exact decimal, never a float);
    /// - `json` is parsed back into a real JSON value (a `jsonb` column returns
    ///   an object/array, not a quoted string);
    /// - `int` becomes a JSON number, `bool` a JSON bool, everything textual a
    ///   JSON string.
    pub fn to_json(&self) -> Value {
        match self {
            SqlValue::Null => Value::Null,
            SqlValue::Bool(b) => Value::Bool(*b),
            SqlValue::Int32(n) => Value::from(*n),
            SqlValue::Int64(n) => Value::from(*n),
            SqlValue::Float64(f) => serde_json::Number::from_f64(*f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            SqlValue::Text(s) | SqlValue::Timestamptz(s) | SqlValue::Uuid(s) => {
                Value::String(s.clone())
            }
            // Exact decimal preserved as a string — honoring the no-float rule.
            SqlValue::Numeric(s) => Value::String(s.clone()),
            // A jsonb column round-trips to a real JSON value.
            SqlValue::Json(s) => {
                serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone()))
            }
            // Not produced for catalog data; represent losslessly as a byte array.
            SqlValue::Bytes(b) => Value::Array(b.iter().map(|x| Value::from(*x)).collect()),
        }
    }

    /// A stable string key for grouping/joining rows on this cell's value
    /// (used by relation expansion to match a foreign key to a primary key).
    pub fn group_key(&self) -> String {
        self.to_json().to_string()
    }
}
