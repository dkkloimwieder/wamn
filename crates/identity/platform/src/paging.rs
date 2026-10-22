//! Scoped keyset cursors for control and identity lists.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_execution_contract::canonical_json_bytes;

use crate::{IdentityError, IdentityErrorType, IdentityRefusal, PrincipalId};

/// The fixed comparison order of a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Order {
    /// Project identifier ascending.
    Project,
    /// Environment name ascending.
    Environment,
    /// Email ascending, then principal UUID ascending.
    Principal,
    /// Application (0), then control (1), each by audience ascending.
    Audience,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    v: u8,
    operation: String,
    scope: Value,
    order: Order,
    key: Value,
}

/// A validated request, bound to the current caller and scope.
#[derive(Debug)]
pub struct ListPage {
    limit: Option<usize>,
    cursor: Cursor,
}

fn invalid(field: &'static str) -> IdentityError {
    IdentityError::refused(
        IdentityErrorType::InvalidInput,
        IdentityRefusal::Invalid(field),
        format!("invalid list {field}"),
    )
}

fn valid_key(order: Order, key: &Value) -> bool {
    let Some(key) = key.as_array() else {
        return false;
    };
    let text = |value: &Value| value.as_str().is_some_and(|text| !text.contains('\0'));
    match (order, key.as_slice()) {
        (Order::Project | Order::Environment, [name]) => text(name),
        (Order::Principal, [email, id]) => {
            text(email)
                && id.as_str().is_some_and(|id| {
                    id.parse::<PrincipalId>()
                        .is_ok_and(|parsed| parsed.as_str() == id)
                })
        }
        (Order::Audience, [kind, audience]) => {
            matches!(kind.as_u64(), Some(0 | 1))
                && text(audience)
                && (kind.as_u64() != Some(1)
                    || audience
                        .as_str()
                        .and_then(crate::control::control_audience_org)
                        .is_some())
        }
        _ => false,
    }
}

impl ListPage {
    /// Parse opt-in paging. Explicit nulls and malformed cursors refuse.
    pub fn new(
        input: &Value,
        operation: &str,
        scope: Value,
        order: Order,
    ) -> Result<Self, IdentityError> {
        let limit = input.get("limit");
        let cursor = input.get("cursor");
        let paged = limit.is_some() || cursor.is_some();
        let limit = match limit {
            Some(value) => Some(
                usize::try_from(
                    value
                        .as_u64()
                        .filter(|limit| (1..=100).contains(limit))
                        .ok_or_else(|| invalid("limit"))?,
                )
                .expect("limit is at most 100"),
            ),
            None => paged.then_some(100),
        };
        let expected = Cursor {
            v: 1,
            operation: operation.into(),
            scope,
            order,
            key: Value::Null,
        };
        let cursor = match cursor {
            None => expected,
            Some(value) => {
                let encoded = value
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| invalid("cursor"))?;
                let bytes = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .map_err(|_| invalid("cursor"))?;
                let decoded: Cursor =
                    serde_json::from_slice(&bytes).map_err(|_| invalid("cursor"))?;
                if decoded.v != 1
                    || decoded.operation != expected.operation
                    || decoded.scope != expected.scope
                    || decoded.order != order
                    || !valid_key(order, &decoded.key)
                    || URL_SAFE_NO_PAD.encode(canonical_json_bytes(
                        &serde_json::to_value(&decoded).expect("cursor serializes"),
                    )) != encoded
                {
                    return Err(invalid("cursor"));
                }
                decoded
            }
        };
        Ok(Self { limit, cursor })
    }

    /// The extra row establishes whether another page exists. Null means all rows.
    pub fn read_limit(&self) -> Option<i64> {
        self.limit
            .map(|limit| i64::try_from(limit + 1).expect("read limit is at most 101"))
    }

    /// The text at a validated key position, or none on the first page.
    pub fn after(&self, index: usize) -> Option<&str> {
        self.cursor.key.get(index).and_then(Value::as_str)
    }

    /// The audience kind, or none on the first page.
    pub fn after_kind(&self) -> Option<u64> {
        self.cursor.key.get(0).and_then(Value::as_u64)
    }

    /// Trim the lookahead row and attach metadata beside the existing row wrapper.
    pub fn answer<T: Serialize>(
        &self,
        name: &str,
        mut rows: Vec<T>,
        key: impl FnOnce(&T) -> Value,
    ) -> Value {
        let more = self.limit.is_some_and(|limit| rows.len() > limit);
        if let Some(limit) = self.limit {
            rows.truncate(limit);
        }
        let next_cursor = if more {
            let cursor = Cursor {
                v: 1,
                operation: self.cursor.operation.clone(),
                scope: self.cursor.scope.clone(),
                order: self.cursor.order,
                key: key(rows.last().expect("a nonempty page precedes lookahead")),
            };
            Some(URL_SAFE_NO_PAD.encode(canonical_json_bytes(
                &serde_json::to_value(&cursor).expect("cursor serializes"),
            )))
        } else {
            None
        };
        let total = if !more && self.cursor.key.is_null() {
            i32::try_from(rows.len()).ok()
        } else {
            None
        };
        json!({name: rows, "next_cursor": next_cursor, "total": total})
    }
}
