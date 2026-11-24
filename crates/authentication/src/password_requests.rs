//! Password request admission and email credential delivery.

use std::{borrow::Cow, net::IpAddr, time::Duration};

use wamn_digest::Digest;
use wamn_identity_protocol::PrincipalId;
use wamn_registry::OrgId;

use crate::identity_error::IdentityError;
use crate::password::{Invitation, Password, PasswordError, PasswordWork};

/// The database operations needed by password requests and invitations.
#[async_trait::async_trait]
pub trait PasswordStore: Send {
    async fn admit(&mut self, issuer: &str, buckets: &[(&str, i32)])
    -> Result<bool, IdentityError>;
    async fn recovery_principal(
        &mut self,
        email: &str,
    ) -> Result<Option<PrincipalId>, IdentityError>;
    async fn enrollment_email(
        &mut self,
        principal: &PrincipalId,
    ) -> Result<Option<String>, IdentityError>;
    async fn invitation_email(
        &mut self,
        org: &OrgId,
        principal: &PrincipalId,
    ) -> Result<Option<String>, IdentityError>;
    async fn enroll(
        &mut self,
        work: &PasswordWork,
        principal: &PrincipalId,
        secret: &str,
        password: Password,
    ) -> Result<(), PasswordError>;
    async fn reset(
        &mut self,
        work: &PasswordWork,
        principal: &PrincipalId,
        secret: &str,
        password: Password,
    ) -> Result<(), PasswordError>;
    async fn issue_reset(
        &mut self,
        principal: &PrincipalId,
        email: &str,
    ) -> Result<Invitation, PasswordError>;
    async fn issue_invitation(
        &mut self,
        principal: &PrincipalId,
    ) -> Result<Invitation, PasswordError>;
    async fn consume_email_token(
        &mut self,
        digest: &Digest,
        delivery: EmailDelivery,
    ) -> Result<(), IdentityError>;
}

/// One request against the issuer's fixed-window counters.
#[derive(Debug, Clone, Copy)]
pub enum Attempt<'a> {
    Source(IpAddr),
    Enrollment(&'a PrincipalId),
    Login(&'a str),
    Recovery(&'a str),
}

pub async fn admit(
    store: &mut impl PasswordStore,
    issuer: &str,
    attempt: Attempt<'_>,
) -> Result<bool, IdentityError> {
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
pub async fn pad_recovery(action: impl Future<Output = ()> + Send) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    let _ = tokio::time::timeout_at(deadline, action).await;
    tokio::time::sleep_until(deadline).await;
}

/// The email credential issued for delivery.
#[derive(Debug, Clone, Copy)]
pub enum EmailDelivery {
    Invitation,
    Recovery,
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

/// An invitation and its authorized delivery address.
#[derive(Debug)]
pub struct AddressedInvitation {
    pub email: String,
    pub token: Invitation,
}

/// Issue an invitation for an active member of the organization.
pub async fn invite(
    store: &mut dyn PasswordStore,
    org: &OrgId,
    principal: &PrincipalId,
) -> Result<AddressedInvitation, DeliveryFailure<'static>> {
    let email = store
        .invitation_email(org, principal)
        .await
        .map_err(|_| DeliveryFailure::Unavailable)?
        .ok_or(DeliveryFailure::UnknownPrincipal)?;
    let token = match store.issue_invitation(principal).await {
        Ok(token) => token,
        Err(error) => {
            return Err(DeliveryFailure::Refused {
                error,
                email: Cow::Owned(email),
            });
        }
    };
    Ok(AddressedInvitation { email, token })
}

/// Issue a recovery credential for an active account.
pub async fn recover<'email>(
    store: &mut dyn PasswordStore,
    email: &'email str,
) -> Result<Invitation, DeliveryFailure<'email>> {
    let principal = store
        .recovery_principal(email)
        .await
        .map_err(|_| DeliveryFailure::Unavailable)?
        .ok_or(DeliveryFailure::UnknownPrincipal)?;
    store
        .issue_reset(&principal, email)
        .await
        .map_err(|error| DeliveryFailure::Refused {
            error,
            email: Cow::Borrowed(email),
        })
}

/// Consume only the credential whose delivery failed.
pub async fn deliver(
    store: &mut dyn PasswordStore,
    token: &Invitation,
    delivery: EmailDelivery,
    send: impl Future<Output = bool> + Send,
) -> bool {
    if send.await {
        return true;
    }
    let digest = wamn_digest::bytes_sha256(token.secret().as_bytes());
    let _ = store.consume_email_token(&digest, delivery).await;
    false
}

/// Replace the password for the active account at the normalized address.
pub async fn reset<'email>(
    store: &mut dyn PasswordStore,
    work: &PasswordWork,
    email: &'email str,
    secret: &str,
    password: String,
) -> Result<(), DeliveryFailure<'email>> {
    let principal = store
        .recovery_principal(email)
        .await
        .map_err(|_| DeliveryFailure::Unavailable)?
        .ok_or(DeliveryFailure::UnknownPrincipal)?;
    let refused = |error| DeliveryFailure::Refused {
        error,
        email: Cow::Borrowed(email),
    };
    let password = Password::new(password).map_err(refused)?;
    store
        .reset(work, &principal, secret, password)
        .await
        .map_err(refused)
}
