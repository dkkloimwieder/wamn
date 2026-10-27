//! The opaque keyset cursor: the closed v1 shape that each package's
//! `generated/contracts/cursor-v1.json` fixes.
//!
//! A cursor is canonical compact JSON of `{direction, field, id, key, v}` in
//! unpadded base64url. A decode refuses anything it would not have minted
//! itself: another version, field or direction, another member, and every
//! spelling that is not the one [`id::encode_with_id`] writes.

use serde_json::Value;
use wamn_execution_contract::{canonical_numeric, canonical_timestamptz};
use wamn_postgres_statements::{Numeric, TimestampTz, Uuid};

use crate::{Invalid, scalar};

const VERSION: u64 = 1;

/// The direction of a sort, shared by its key and its `id` tie-breaker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Ascending,
    Descending,
}

impl Direction {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ascending => "ascending",
            Self::Descending => "descending",
        }
    }
}

/// A sort key the cursor carries, as the key's own column type spells it.
pub trait Key: Sized {
    /// The key's JSON, or [`Invalid`] when a row holds a value outside its
    /// type's one spelling.
    fn to_json(&self) -> Result<Value, Invalid>;
    /// The key a cursor carries, only in the spelling [`Key::to_json`] writes.
    fn from_json(value: &Value) -> Result<Self, Invalid>;
}

impl Key for String {
    fn to_json(&self) -> Result<Value, Invalid> {
        Ok(Value::String(self.clone()))
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        value.as_str().map(str::to_owned).ok_or(Invalid)
    }
}

impl Key for Uuid {
    fn to_json(&self) -> Result<Value, Invalid> {
        Ok(Value::String(scalar::uuid(&self.0)?.0))
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        let value = value.as_str().ok_or(Invalid)?;
        scalar::uuid(value)
            .ok()
            .filter(|parsed| parsed.0 == value)
            .ok_or(Invalid)
    }
}

impl Key for TimestampTz {
    fn to_json(&self) -> Result<Value, Invalid> {
        canonical_timestamptz(&self.0)
            .map(Value::String)
            .ok_or(Invalid)
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        let value = value.as_str().ok_or(Invalid)?;
        canonical_timestamptz(value)
            .filter(|canonical| canonical == value)
            .map(TimestampTz)
            .ok_or(Invalid)
    }
}

impl Key for Numeric {
    fn to_json(&self) -> Result<Value, Invalid> {
        canonical_numeric(&self.0).map(Value::String).ok_or(Invalid)
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        let value = value.as_str().ok_or(Invalid)?;
        canonical_numeric(value)
            .filter(|canonical| canonical == value)
            .map(Numeric)
            .ok_or(Invalid)
    }
}

impl Key for i64 {
    fn to_json(&self) -> Result<Value, Invalid> {
        Ok(Value::from(*self))
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        value.as_i64().ok_or(Invalid)
    }
}

impl Key for i32 {
    fn to_json(&self) -> Result<Value, Invalid> {
        Ok(Value::from(*self))
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        value
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .ok_or(Invalid)
    }
}

impl Key for bool {
    fn to_json(&self) -> Result<Value, Invalid> {
        Ok(Value::Bool(*self))
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        value.as_bool().ok_or(Invalid)
    }
}

pub mod id;
