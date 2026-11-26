//! Password request admission and email credential delivery.

use std::{borrow::Cow, net::IpAddr, time::Duration};

use wamn_identity_protocol::{PlatformComponent, PrincipalId};
use wamn_registry::OrgId;

use crate::authentication_event::{Event, Outcome};
use crate::identity::{
    issuer::{Admission, Issuer},
    password::{issue_invitation, issue_reset},
};
use crate::password::{Invitation, Password, PasswordError, PasswordWork};
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

/// A committed invitation and the address authorized to receive it.
#[derive(Debug)]
pub struct AddressedInvitation {
    pub email: String,
    pub token: Invitation,
}

/// Issue an invitation for an active member of the organization.
pub async fn invite(
    store: &mut Issuer<'_>,
    org: &OrgId,
    principal: &PrincipalId,
    event: &mut Event,
) -> Result<AddressedInvitation, DeliveryFailure<'static>> {
    let email = store
        .invitation_email(org, principal, event)
        .await
        .map_err(|_| DeliveryFailure::Unavailable)?
        .ok_or(DeliveryFailure::UnknownPrincipal)?;
    let actor = PlatformComponent::IdentityService.principal_id();
    match issue_invitation(store.0, &actor, principal).await {
        Ok(token) => Ok(AddressedInvitation { email, token }),
        Err(error) => Err(DeliveryFailure::Refused {
            error,
            email: Cow::Owned(email),
        }),
    }
}

/// Issue a recovery credential for an active account.
pub async fn recover<'email>(
    store: &mut Issuer<'_>,
    email: &'email str,
    event: &mut Event,
) -> Result<Invitation, DeliveryFailure<'email>> {
    let principal = store
        .recovery_principal(email, event)
        .await
        .map_err(|_| DeliveryFailure::Unavailable)?
        .ok_or(DeliveryFailure::UnknownPrincipal)?;
    let actor = PlatformComponent::IdentityService.principal_id();
    issue_reset(store.0, &actor, &principal, email)
        .await
        .map_err(|error| DeliveryFailure::Refused {
            error,
            email: Cow::Borrowed(email),
        })
}

/// Persistence of the terminal delivery decision failed.
#[derive(Debug)]
pub struct DeliveryAppendFailure {
    pub cleanup_failed: bool,
}

/// Record the result of a completed mail send. Invalidate the token if the
/// send or the append fails. The issuer commits each write independently.
pub async fn deliver(
    store: &mut Issuer<'_>,
    token: &Invitation,
    accepted: bool,
    event: &mut Event,
) -> Result<Outcome, DeliveryAppendFailure> {
    let write_timeout = Duration::from_secs(1);
    let outcome = if accepted {
        Outcome::DeliveryAccepted
    } else if matches!(
        tokio::time::timeout(write_timeout, store.consume_email_token(token)).await,
        Ok(Ok(()))
    ) {
        Outcome::DeliveryFailed
    } else {
        Outcome::DeliveryCleanupFailed
    };
    // This request owns its append. The reply path cannot retry an uncertain commit.
    event.outcome = None;
    if matches!(
        tokio::time::timeout(write_timeout, event.append_standalone(store.0, outcome)).await,
        Ok(Ok(()))
    ) {
        return Ok(outcome);
    }
    let cleanup_failed = outcome != Outcome::DeliveryFailed
        && !matches!(
            tokio::time::timeout(write_timeout, store.consume_email_token(token)).await,
            Ok(Ok(()))
        );
    Err(DeliveryAppendFailure { cleanup_failed })
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
