//! The HTTPS adapter used by control for identity user management.

use std::{fmt, path::Path};

use anyhow::Context as _;
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use url::Url;
use wamn_identity_protocol::PrincipalId;
use wamn_identity_protocol::wire::{PrincipalRequest, UserRequest, UserResponse};
use wamn_http_client::HttpResponse;

use super::{IdentityHttp, TOKEN_PATH, service_endpoint};

const MAX_USER_RESPONSE_BYTES: usize = 4096;

/// One authenticated client for the user management routes of identity.
pub struct UserClient {
    http: IdentityHttp,
    create: Url,
    invite: Url,
    disable: Url,
}

impl fmt::Debug for UserClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("UserClient").finish_non_exhaustive()
    }
}

/// The result of a request to disable a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisableOutcome {
    /// Identity disabled the principal or had already disabled it.
    Disabled,
    /// The principal is not a user of the control org.
    UserNotFound,
    /// Another org has an active membership for the user.
    UserInAnotherOrg,
}

impl UserClient {
    /// Use the projected control service account token and identity server CA.
    pub fn new(endpoint: &str, ca: &Path) -> anyhow::Result<Self> {
        Self::with_token_file(endpoint, ca, Path::new(TOKEN_PATH))
    }

    pub(super) fn with_token_file(endpoint: &str, ca: &Path, token: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            create: service_endpoint(endpoint, "users")?,
            invite: service_endpoint(endpoint, "invitations")?,
            disable: service_endpoint(endpoint, "users/disable")?,
            http: IdentityHttp::new(ca, token)?,
        })
    }

    /// Create or reuse one active user identity.
    pub async fn create(&self, email: &str, display_name: &str) -> anyhow::Result<UserResponse> {
        let response = self
            .http
            .post(
                &self.create,
                encode(&UserRequest {
                    email,
                    display_name,
                })?,
            )
            .await?;
        let status = response.status();
        let body = read_response(response).await?;
        if matches!(status, StatusCode::BAD_REQUEST | StatusCode::CONFLICT)
            && let Ok(refusal) = serde_json::from_slice::<RefusalBody>(&body)
        {
            return Err(UserRefused(refusal.error).into());
        }
        anyhow::ensure!(
            status == StatusCode::OK,
            "identity did not create the user: {status}"
        );
        serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!("identity user reply is invalid"))
    }

    /// Ask identity to mail an invitation to the user's stored address.
    pub async fn invite(&self, principal_id: &PrincipalId) -> anyhow::Result<()> {
        let response = self
            .http
            .post(&self.invite, invitation_body(principal_id)?)
            .await?;
        let status = response.status();
        anyhow::ensure!(
            status == StatusCode::CREATED,
            "identity did not accept the invitation for delivery: {status}"
        );
        // Drain the bounded response so the HTTPS connection can be reused.
        read_response(response).await?;
        Ok(())
    }

    /// Disable a user's principal in identity.
    pub async fn disable(&self, principal_id: &PrincipalId) -> anyhow::Result<DisableOutcome> {
        let response = self
            .http
            .post(
                &self.disable,
                encode(&PrincipalRequest {
                    principal_id: *principal_id,
                })?,
            )
            .await?;
        let status = response.status();
        let body = read_response(response).await?;
        let refusal = serde_json::from_slice::<RefusalBody>(&body).ok();
        match (status, refusal.as_ref().map(|body| body.error.as_str())) {
            (StatusCode::NO_CONTENT, _) => Ok(DisableOutcome::Disabled),
            (StatusCode::NOT_FOUND, Some("user_not_found")) => Ok(DisableOutcome::UserNotFound),
            (StatusCode::CONFLICT, Some("user_in_another_org")) => {
                Ok(DisableOutcome::UserInAnotherOrg)
            }
            _ => anyhow::bail!("identity did not disable the user: {status}"),
        }
    }
}

fn encode(body: &impl Serialize) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec(body).context("encode identity user management request")
}

pub(super) fn invitation_body(principal_id: &PrincipalId) -> anyhow::Result<Vec<u8>> {
    encode(&PrincipalRequest {
        principal_id: *principal_id,
    })
}

async fn read_response(mut response: HttpResponse) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("read identity user management reply")?
    {
        anyhow::ensure!(
            chunk.len() <= MAX_USER_RESPONSE_BYTES - body.len(),
            "identity user management reply exceeds the size limit"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Deserialize)]
struct RefusalBody {
    error: String,
}

/// Identity refused the user, with identity's reason: for example an email
/// that the identity rules refuse, or a disabled user.
#[derive(Debug)]
pub struct UserRefused(pub String);

impl fmt::Display for UserRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "identity refused the user: {}", self.0)
    }
}

impl std::error::Error for UserRefused {}
