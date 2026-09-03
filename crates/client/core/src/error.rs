//! What a caller can act on when a request does not succeed.
//!
//! Modelled from the operation's declared error contract, not invented here:
//! the literals and their detail members come from `*.errors.json`, so a new
//! case appears by regenerating rather than by editing this file.
//!
//! # Two distinctions the spec makes load-bearing
//!
//! **`401` is indistinguishable; `403` names the token.** An unauthenticated
//! response must not tell a caller whether the credential was unknown,
//! expired, or merely wrong — those answers are an oracle. An authorization
//! failure is different: the caller IS authenticated, and telling them which
//! grant they lack is how they fix it.
//!
//! **`concurrency_conflict` carries BOTH revisions.** The expected one alone
//! says a write lost; with the observed one a caller can show what changed
//! underneath them, which is the difference between a retry and a merge.

use serde::{Deserialize, Serialize};

/// A failed request, as a caller should reason about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// The credential was not accepted. Deliberately carries NO detail.
    Unauthenticated,
    /// The caller is authenticated but lacks a grant, which is named.
    PermissionDenied {
        /// The operation the caller may not invoke.
        operation: String,
    },
    /// A write raced another and lost, carrying both revisions.
    ConcurrencyConflict {
        /// The revision the caller wrote against.
        expected_row_version: i64,
        /// The revision the row actually carries now.
        observed_row_version: i64,
    },
    /// A typed refusal from the operation's contract.
    Operation {
        /// The contract literal, e.g. `invalid_input`.
        literal: String,
        /// The detail members the contract declares for it.
        detail: serde_json::Value,
    },
    /// The transport failed before any contract applied.
    Transport {
        /// What went wrong, for a log rather than a branch.
        detail: String,
    },
    /// A response did not match the envelope the operation declares.
    MalformedResponse {
        /// What was expected and what arrived.
        detail: String,
    },
}

impl ClientError {
    /// Stable code a caller branches on.
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::PermissionDenied { .. } => "permission_denied",
            Self::ConcurrencyConflict { .. } => "concurrency_conflict",
            Self::Operation { literal, .. } => literal,
            Self::Transport { .. } => "transport",
            Self::MalformedResponse { .. } => "malformed_response",
        }
    }

    /// Build from one envelope item's error member.
    #[must_use]
    pub fn from_item_error(error: &serde_json::Value) -> Self {
        let literal = error
            .get("code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("internal_error");
        let detail = error
            .get("detail")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match literal {
            "permission_denied" => Self::PermissionDenied {
                operation: detail
                    .get("operation")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },
            "concurrency_conflict" => {
                let revision = |name: &str| {
                    detail
                        .get(name)
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                };
                Self::ConcurrencyConflict {
                    expected_row_version: revision("expected_row_version"),
                    observed_row_version: revision("observed_row_version"),
                }
            }
            other => Self::Operation {
                literal: other.to_owned(),
                detail,
            },
        }
    }

    /// Build from a transport status that never reached the contract.
    #[must_use]
    pub fn from_status(status: u16, body: &str) -> Self {
        match status {
            // No detail, deliberately: distinguishing "unknown token" from
            // "expired token" hands an attacker an oracle.
            401 => Self::Unauthenticated,
            403 => Self::PermissionDenied {
                operation: serde_json::from_str::<serde_json::Value>(body)
                    .ok()
                    .and_then(|body| {
                        body.get("operation")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_default(),
            },
            other => Self::Transport {
                detail: format!("status {other}"),
            },
        }
    }
}

impl core::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unauthenticated => formatter.write_str("the credential was not accepted"),
            Self::PermissionDenied { operation } => {
                write!(formatter, "not permitted to invoke {operation:?}")
            }
            Self::ConcurrencyConflict {
                expected_row_version,
                observed_row_version,
            } => write!(
                formatter,
                "the row moved: wrote against revision {expected_row_version}, it now carries \
                 {observed_row_version}"
            ),
            Self::Operation { literal, .. } => write!(formatter, "{literal}"),
            Self::Transport { detail } => write!(formatter, "transport failed: {detail}"),
            Self::MalformedResponse { detail } => {
                write!(formatter, "malformed response: {detail}")
            }
        }
    }
}

impl std::error::Error for ClientError {}

/// One envelope item's outcome, correlated by `request_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemOutcome {
    /// Echoes the request item's `request_id`.
    pub request_id: String,
    /// Present when the item succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Present when the item was refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
}

impl ItemOutcome {
    /// The item's value, or the typed error it carried.
    ///
    /// # Errors
    ///
    /// [`ClientError`] built from the item's declared error contract.
    pub fn into_result(self) -> Result<serde_json::Value, ClientError> {
        match (self.value, self.error) {
            (_, Some(error)) => Err(ClientError::from_item_error(&error)),
            (Some(value), None) => Ok(value),
            (None, None) => Err(ClientError::MalformedResponse {
                detail: format!("item {:?} carried neither value nor error", self.request_id),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// EXIT GATE: a stale row_version yields concurrency_conflict carrying
    /// BOTH revisions — the expected one alone cannot tell a caller whether to
    /// retry or merge.
    #[test]
    fn a_stale_revision_carries_both_versions() {
        let error = ClientError::from_item_error(&json!({
            "code": "concurrency_conflict",
            "detail": { "expected_row_version": 4, "observed_row_version": 7 },
        }));
        assert_eq!(
            error,
            ClientError::ConcurrencyConflict {
                expected_row_version: 4,
                observed_row_version: 7,
            }
        );
        let rendered = error.to_string();
        assert!(
            rendered.contains('4') && rendered.contains('7'),
            "{rendered}"
        );
    }

    /// EXIT GATE: 401 is indistinguishable, 403 names the token's shortfall.
    #[test]
    fn authentication_is_opaque_while_authorization_is_specific() {
        for body in [
            "{}",
            r#"{"reason":"token expired"}"#,
            r#"{"reason":"unknown token"}"#,
        ] {
            assert_eq!(
                ClientError::from_status(401, body),
                ClientError::Unauthenticated,
                "a 401 must not distinguish why: {body}"
            );
        }
        assert_eq!(
            ClientError::from_status(403, r#"{"operation":"purchase_order.update"}"#),
            ClientError::PermissionDenied {
                operation: "purchase_order.update".to_owned(),
            }
        );
    }

    /// The 401 arm carries no detail AT ALL — not merely a different message.
    #[test]
    fn an_unauthenticated_error_renders_nothing_about_the_credential() {
        let rendered =
            ClientError::from_status(401, r#"{"reason":"expired at 2026-01-01"}"#).to_string();
        assert!(!rendered.contains("expired"), "{rendered}");
        assert!(!rendered.contains("2026"), "{rendered}");
    }

    #[test]
    fn a_contract_literal_passes_through_with_its_detail() {
        let error = ClientError::from_item_error(&json!({
            "code": "invalid_input",
            "detail": { "field": "value.quantity", "minimum": 1 },
        }));
        assert_eq!(error.code(), "invalid_input");
        match error {
            ClientError::Operation { detail, .. } => {
                assert_eq!(detail["field"], "value.quantity");
            }
            other => panic!("expected an operation error, got {other:?}"),
        }
    }

    /// An item carrying neither value nor error is malformed, not empty —
    /// reading it as a successful null is how a caller silently loses a write.
    #[test]
    fn an_item_with_neither_value_nor_error_is_malformed() {
        let outcome = ItemOutcome {
            request_id: "r1".to_owned(),
            value: None,
            error: None,
        };
        assert!(matches!(
            outcome.into_result(),
            Err(ClientError::MalformedResponse { .. })
        ));
    }
}
