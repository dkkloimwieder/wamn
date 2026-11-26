//! Bound keyset cursors for control and identity lists.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_digest::canonical_json_bytes;

use crate::identity_error::{IdentityError, IdentityErrorType, IdentityRefusal};
use wamn_identity_protocol::PrincipalId;

/// The fixed comparison order of a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Order {
    /// Project identifier ascending.
    Project,
    /// Environment name ascending.
    #[serde(rename = "Environment")]
    Env,
    /// Email ascending, then principal UUID ascending.
    Principal,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    v: u8,
    operation: String,
    binding: Value,
    order: Order,
    key: Value,
}

/// A validated request, bound to the current caller and binding.
#[derive(Debug)]
pub struct ListPage {
    limit: Option<usize>,
    cursor: Cursor,
    after_principal: Option<PrincipalId>,
}

fn invalid(field: &'static str) -> IdentityError {
    IdentityError::refused(
        IdentityErrorType::InvalidInput,
        IdentityRefusal::Invalid(field),
        format!("invalid list {field}"),
    )
}

fn parse_key(order: Order, key: &Value) -> Result<Option<PrincipalId>, IdentityError> {
    let key = key.as_array().ok_or_else(|| invalid("cursor"))?;
    let text = |value: &Value| value.as_str().is_some_and(|text| !text.contains('\0'));
    let valid = match (order, key.as_slice()) {
        (Order::Project | Order::Env, [name]) => text(name),
        (Order::Principal, [email, id]) if text(email) => {
            return id
                .as_str()
                .ok_or_else(|| invalid("cursor"))?
                .parse()
                .map(Some)
                .map_err(|_| invalid("cursor"));
        }
        _ => false,
    };
    if valid {
        Ok(None)
    } else {
        Err(invalid("cursor"))
    }
}

impl ListPage {
    /// Parse opt-in paging. Explicit nulls and malformed cursors refuse.
    pub fn new(
        input: &Value,
        operation: &str,
        binding: Value,
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
            binding,
            order,
            key: Value::Null,
        };
        let (cursor, after_principal) = match cursor {
            None => (expected, None),
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
                    || decoded.binding != expected.binding
                    || decoded.order != order
                    || URL_SAFE_NO_PAD.encode(canonical_json_bytes(
                        &serde_json::to_value(&decoded).expect("cursor serializes"),
                    )) != encoded
                {
                    return Err(invalid("cursor"));
                }
                let principal = parse_key(order, &decoded.key)?;
                (decoded, principal)
            }
        };
        Ok(Self {
            limit,
            cursor,
            after_principal,
        })
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

    /// The principal at the end of the previous page.
    pub fn after_principal(&self) -> Option<&PrincipalId> {
        self.after_principal.as_ref()
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
                binding: self.cursor.binding.clone(),
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
