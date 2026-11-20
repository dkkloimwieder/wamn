//! Shared host route refusals and JSON payload parsing.

use serde_json::Value;
use wamn_engine::native_routes::DeliveryError;

/// Why a host route refused a request.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// A delivery refusal, such as a caller without `admin`.
    Delivery(DeliveryError),
    /// The code and detail declared by the route contract for this request element.
    Declared { code: &'static str, detail: Value },
    /// A failure of the host or of a service it calls.
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for Refusal {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

impl From<sqlx::Error> for Refusal {
    fn from(error: sqlx::Error) -> Self {
        Self::Failed(error.into())
    }
}

pub(super) fn parse<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, Refusal> {
    serde_json::from_str(payload).map_err(|_| Refusal::Delivery(DeliveryError::InvalidPayload))
}
