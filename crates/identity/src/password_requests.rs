//! Password request admission and email credential delivery.

use std::{borrow::Cow, fmt, net::IpAddr, time::Duration};

use sqlx::{Postgres, Transaction};
use wamn_identity_protocol::{PlatformComponent, PrincipalId};
use wamn_registry::OrgId;

use crate::authentication_event::{Event, Outcome, commit_success};
use crate::identity::{
    issuer::{Admission, Issuer},
    password::prepare_email_token,
};
use crate::password::{
    INVITATION_PURPOSE, Invitation, Password, PasswordError, PasswordWork, RESET_PURPOSE,
};
use wamn_authentication::identity_error::IdentityError;

/// One request against the issuer's fixed-window counters.
#[derive(Debug, Clone, Copy)]
pub enum Attempt<'a> {
    Source(IpAddr),
    Enrollment(&'a PrincipalId),
    Login(&'a str),
    Recovery(&'a str),
}

pub async fn admit(
    store: &mut Issuer<'_>,
    issuer: &str,
    attempt: Attempt<'_>,
) -> Result<Admission, IdentityError> {
    match attempt {
        Attempt::Source(source) => {
            let source = match source {
                IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(source, IpAddr::V4),
                IpAddr::V4(_) => source,
            };
            let key = format!("source:{source}");
            store.admit(issuer, &[("global", 120), (&key, 20)]).await
        }
        Attempt::Enrollment(principal) => {
            store
                .admit(issuer, &[(&format!("enroll:{principal}"), 5)])
                .await
        }
        Attempt::Login(email) | Attempt::Recovery(email) => {
            let prefix = if matches!(attempt, Attempt::Login(_)) {
                "login"
            } else {
                "recovery"
            };
            let hash = wamn_digest::bytes_sha256(email.as_bytes()).hex();
            store
                .admit(issuer, &[(&format!("{prefix}:{hash}"), 5)])
                .await
        }
    }
}

/// Normalize a request address after its original byte length is checked.
pub fn password_email(email: &str) -> Option<String> {
    (email.len() <= 320).then(|| email.trim().to_lowercase())
}

/// Pad the account-dependent recovery path without extending it beyond the deadline.
pub async fn pad_recovery(action: impl Future<Output = ()> + Send) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    let completed = tokio::time::timeout_at(deadline, action).await.is_ok();
    tokio::time::sleep_until(deadline).await;
    completed
}

/// Result of issuing a one-time email credential.
#[derive(Debug)]
pub enum DeliveryFailure<'a> {
    Unavailable,
    UnknownPrincipal,
    Refused {
        error: PasswordError,
        email: Cow<'a, str>,
    },
}

/// An email credential whose issuance and successful delivery decision commit together.
pub struct PendingDelivery<'connection, 'email> {
    tx: Transaction<'connection, Postgres>,
    pub email: Cow<'email, str>,
    pub token: Invitation,
}

impl fmt::Debug for PendingDelivery<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingDelivery")
            .finish_non_exhaustive()
    }
}

impl PendingDelivery<'_, '_> {
    /// Commit accepted delivery with its event, or consume the refused credential before auditing.
    pub async fn finish(mut self, accepted: bool, event: &mut Event) -> bool {
        if accepted {
            return commit_success(self.tx, Some(event), Outcome::DeliveryAccepted, None)
                .await
                .is_ok();
        }
        let digest = wamn_digest::bytes_sha256(self.token.secret().as_bytes());
        let consumed = sqlx::query(
            "UPDATE identity.password_token SET consumed_at = clock_timestamp()
             WHERE token_hash = $1 AND consumed_at IS NULL",
        )
        .bind(digest.to_string())
        .execute(&mut *self.tx)
        .await
        .is_ok()
            && self.tx.commit().await.is_ok();
        event.outcome = Some(if consumed {
            Outcome::DeliveryFailed
        } else {
            Outcome::DeliveryCleanupFailed
        });
        false
    }
}

/// Prepare an invitation for an active member of the organization.
pub async fn invite<'connection>(
    store: &'connection mut Issuer<'_>,
    org: &OrgId,
    principal: &PrincipalId,
    event: &mut Event,
) -> Result<PendingDelivery<'connection, 'static>, DeliveryFailure<'static>> {
    let email = store
        .invitation_email(org, principal, event)
        .await
        .map_err(|_| DeliveryFailure::Unavailable)?
        .ok_or(DeliveryFailure::UnknownPrincipal)?;
    prepare(store, principal, Cow::Owned(email), None).await
}

/// Prepare a recovery credential for an active account.
pub async fn recover<'connection, 'email>(
    store: &'connection mut Issuer<'_>,
    email: &'email str,
    event: &mut Event,
) -> Result<PendingDelivery<'connection, 'email>, DeliveryFailure<'email>> {
    let principal = store
        .recovery_principal(email, event)
        .await
        .map_err(|_| DeliveryFailure::Unavailable)?
        .ok_or(DeliveryFailure::UnknownPrincipal)?;
    prepare(store, &principal, Cow::Borrowed(email), Some(email)).await
}

async fn prepare<'connection, 'email>(
    store: &'connection mut Issuer<'_>,
    principal: &PrincipalId,
    email: Cow<'email, str>,
    expected_email: Option<&str>,
) -> Result<PendingDelivery<'connection, 'email>, DeliveryFailure<'email>> {
    let purpose = if expected_email.is_some() {
        RESET_PURPOSE
    } else {
        INVITATION_PURPOSE
    };
    match prepare_email_token(
        store.0,
        &PlatformComponent::IdentityService.principal_id(),
        principal,
        purpose,
        expected_email,
    )
    .await
    {
        Ok((tx, token)) => Ok(PendingDelivery { tx, email, token }),
        Err(error) => Err(DeliveryFailure::Refused { error, email }),
    }
}

/// Replace the password for the active account at the normalized address.
pub async fn reset<'email>(
    store: &mut Issuer<'_>,
    work: &PasswordWork,
    email: &'email str,
    secret: &str,
    password: String,
    event: &mut Event,
) -> Result<(), DeliveryFailure<'email>> {
    let principal = store
        .recovery_principal(email, event)
        .await
        .map_err(|_| DeliveryFailure::Unavailable)?
        .ok_or(DeliveryFailure::UnknownPrincipal)?;
    let refused = |error| DeliveryFailure::Refused {
        error,
        email: Cow::Borrowed(email),
    };
    let password = Password::new(password).map_err(refused)?;
    store
        .reset(work, &principal, secret, password, event)
        .await
        .map_err(refused)
}
