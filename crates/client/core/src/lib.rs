//! Transport for published WAMN routes.
//!
//! Owns the four things generated code must not: the deployment base URL, the
//! credential provider, route construction, and the operation-driven envelope,
//! error and paging semantics.
//!
//! # Why these live here and not in generated code
//!
//! A generated client names an OPERATION. Everything about WHERE that
//! operation is reached — host, base URL, path — is deployment fact, and code
//! generated from a package contract cannot know it without being regenerated
//! per deployment. Route metadata is therefore supplied to this layer
//! (spec ruling 3).
//!
//! Transport semantics are operation-driven for the same reason in reverse:
//! the envelope, `per_input` behaviour, paging and error detail come from each
//! operation's declared contract, so this layer applies what the contract says
//! rather than assuming one shape globally (spec ruling 4).

pub mod credentials;
pub mod cursor;
pub mod descriptor;
pub mod error;
pub mod route;

use std::collections::BTreeMap;
use std::sync::Arc;

pub use credentials::{CredentialProvider, StaticPat};
pub use cursor::Cursor;
pub use descriptor::FieldDescriptor;
pub use error::{ClientError, ItemOutcome};
pub use route::{RouteError, RouteMetadata};

/// One HTTP exchange, as this client needs it.
///
/// A seam, so the envelope and error semantics above can be proven BELOW the
/// terminal layer without a live server — which is what the slice's exit gate
/// requires.
#[async_trait::async_trait]
pub trait Transport: Send + Sync + core::fmt::Debug {
    /// Send one request and return its status and body.
    ///
    /// # Errors
    ///
    /// A transport-level failure, before any contract applies.
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError>;
}

/// One outbound request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// Absolute URL.
    pub url: String,
    /// HTTP method.
    pub method: String,
    /// Header name to value, ordered so a request is reproducible.
    pub headers: BTreeMap<String, String>,
    /// Canonical request body.
    pub body: Vec<u8>,
}

/// One response, before the contract is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status.
    pub status: u16,
    /// Response body.
    pub body: String,
}

/// A client bound to one deployment.
#[derive(Debug)]
pub struct WamnClient {
    base_url: String,
    host: Option<String>,
    credentials: Arc<dyn CredentialProvider>,
    transport: Arc<dyn Transport>,
}

impl WamnClient {
    /// Bind to one deployment.
    ///
    /// Base URL, host and credential provider are construction-time, not
    /// per-call: a client that could be pointed elsewhere mid-flight would let
    /// one screen's requests reach two deployments.
    ///
    /// `host` is the header the deployment routes on, when it routes by host.
    /// It sits HERE and not on a route because a release does not record one —
    /// publication refuses an authored `route.host` and the deployment stamps
    /// it in at mint — so it is deployment config exactly like the base URL.
    #[must_use]
    pub fn new(
        base_url: impl Into<String>,
        host: Option<String>,
        credentials: Arc<dyn CredentialProvider>,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            host,
            credentials,
            transport,
        }
    }

    /// Invoke one operation with an array envelope.
    ///
    /// `items` are the envelope's request objects; each MUST carry a
    /// `request_id`, which is how outcomes correlate back. Returns one outcome
    /// per item, in request order.
    ///
    /// # Errors
    ///
    /// [`ClientError`] for a transport failure, an authentication or
    /// authorization refusal, or a response that does not match the envelope.
    pub async fn invoke(
        &self,
        route: &RouteMetadata,
        parameters: &BTreeMap<String, String>,
        items: &[serde_json::Value],
    ) -> Result<Vec<ItemOutcome>, ClientError> {
        let path = route
            .path(parameters)
            .map_err(|error| ClientError::Operation {
                literal: error.code().to_owned(),
                detail: serde_json::json!({ "detail": error.to_string() }),
            })?;
        let bearer = self
            .credentials
            .bearer()
            .await
            .map_err(|error| ClientError::Transport {
                detail: error.to_string(),
            })?;

        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_owned(), "application/json".to_owned());
        headers.insert("authorization".to_owned(), format!("Bearer {bearer}"));
        if let Some(host) = &self.host {
            headers.insert("host".to_owned(), host.clone());
        }

        let body = wamn_execution_contract::canonical_json_bytes(&serde_json::Value::Array(
            items.to_vec(),
        ));
        let response = self
            .transport
            .send(HttpRequest {
                url: format!("{}{path}", self.base_url),
                method: route.method.clone(),
                headers,
                body,
            })
            .await?;

        if response.status != 200 {
            return Err(ClientError::from_status(response.status, &response.body));
        }
        let outcomes: Vec<ItemOutcome> = serde_json::from_str(&response.body).map_err(|error| {
            ClientError::MalformedResponse {
                detail: format!("response is not an outcome array: {error}"),
            }
        })?;
        if outcomes.len() != items.len() {
            return Err(ClientError::MalformedResponse {
                detail: format!(
                    "sent {} items and received {} outcomes",
                    items.len(),
                    outcomes.len()
                ),
            });
        }
        Ok(outcomes)
    }
}
