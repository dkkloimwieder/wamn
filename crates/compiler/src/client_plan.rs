use crate::client_ir::leaf_fields;

/// Contract input paths that carry a platform value instead of operator input.
const SUPPLIED_PATHS: [(&str, SuppliedType); 5] = [
    ("request_id", SuppliedType::RequestId),
    ("idempotency_key", SuppliedType::IdempotencyKey),
    ("value.idempotency_key", SuppliedType::IdempotencyKey),
    ("occurred_at", SuppliedType::OccurredAt),
    ("value.occurred_at", SuppliedType::OccurredAt),
];

/// The request element field that carries the idempotency key of one operation, from its
/// generated input contract, or `None` when the contract declares no key.
///
/// Publish writes it into the operation's route, so the route host and a
/// session driver read the same reserved path.
#[must_use]
pub fn idempotency_field(input_contract: &serde_json::Value) -> Option<String> {
    let fields = crate::client_fields::input_fields_of(input_contract);
    leaf_fields(&fields)
        .into_iter()
        .find(|field| {
            SUPPLIED_PATHS
                .iter()
                .any(|(path, type_)| *type_ == SuppliedType::IdempotencyKey && *path == field.path)
        })
        .map(|field| field.path.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppliedType {
    /// One submission attempt's identity.
    RequestId,
    /// The replay key of one submission intent.
    IdempotencyKey,
    /// The time at which the operator started the intent.
    OccurredAt,
}
