//! Terminal identity decisions and the append-only system database writer.

use std::{fmt, net::IpAddr};

use sqlx::{Connection as _, PgConnection};
use uuid::Uuid;
use wamn_identity_protocol::PrincipalId;
use wamn_registry::OrgId;

use crate::password::{PasswordError, PasswordErrorType, RefusalCause};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventType {
    PasswordSignIn,
    SessionRenew,
    SessionSignOut,
    SessionSignOutAll,
    PasswordRecoveryRequest,
    PasswordReset,
    InvitationRequest,
    PasswordEnroll,
    NatsWorkloadCredentialIssue,
    NatsHostCredentialIssue,
}

impl EventType {
    pub fn code(self) -> &'static str {
        match self {
            Self::PasswordSignIn => "password_sign_in",
            Self::SessionRenew => "session_renew",
            Self::SessionSignOut => "session_sign_out",
            Self::SessionSignOutAll => "session_sign_out_all",
            Self::PasswordRecoveryRequest => "password_recovery_request",
            Self::PasswordReset => "password_reset",
            Self::InvitationRequest => "invitation_request",
            Self::PasswordEnroll => "password_enroll",
            Self::NatsWorkloadCredentialIssue => "nats_workload_credential_issue",
            Self::NatsHostCredentialIssue => "nats_host_credential_issue",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    DeliveryAccepted,
    RequestInvalid,
    EmailInvalid,
    PasswordPolicyRefused,
    UnknownEmail,
    BadPassword,
    PrincipalUnknown,
    PrincipalInactive,
    CredentialMissing,
    CredentialCorrupt,
    AudienceInvalid,
    OrgUnknown,
    OrgInactive,
    MembershipMissing,
    MembershipInactive,
    GlobalRateLimited,
    SourceRateLimited,
    AccountRateLimited,
    RequestBusy,
    PasswordWorkBusy,
    TokenMalformed,
    TokenUnknown,
    TokenConsumed,
    TokenExpired,
    RenewalInactive,
    RenewalRevoked,
    RenewalReused,
    AlreadyEnrolled,
    AddressChanged,
    DeliveryFailed,
    DeliveryCleanupFailed,
    WorkloadRefused,
    OrgMismatch,
    PublicKeyInvalid,
    NatsAccountMissing,
    SigningFailed,
    Unavailable,
    TimedOut,
}

impl Outcome {
    pub fn code(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::DeliveryAccepted => "delivery_accepted",
            Self::RequestInvalid => "request_invalid",
            Self::EmailInvalid => "email_invalid",
            Self::PasswordPolicyRefused => "password_policy_refused",
            Self::UnknownEmail => "unknown_email",
            Self::BadPassword => "bad_password",
            Self::PrincipalUnknown => "principal_unknown",
            Self::PrincipalInactive => "principal_inactive",
            Self::CredentialMissing => "credential_missing",
            Self::CredentialCorrupt => "credential_corrupt",
            Self::AudienceInvalid => "audience_invalid",
            Self::OrgUnknown => "org_unknown",
            Self::OrgInactive => "org_inactive",
            Self::MembershipMissing => "membership_missing",
            Self::MembershipInactive => "membership_inactive",
            Self::GlobalRateLimited => "global_rate_limited",
            Self::SourceRateLimited => "source_rate_limited",
            Self::AccountRateLimited => "account_rate_limited",
            Self::RequestBusy => "request_busy",
            Self::PasswordWorkBusy => "password_work_busy",
            Self::TokenMalformed => "token_malformed",
            Self::TokenUnknown => "token_unknown",
            Self::TokenConsumed => "token_consumed",
            Self::TokenExpired => "token_expired",
            Self::RenewalInactive => "renewal_inactive",
            Self::RenewalRevoked => "renewal_revoked",
            Self::RenewalReused => "renewal_reused",
            Self::AlreadyEnrolled => "already_enrolled",
            Self::AddressChanged => "address_changed",
            Self::DeliveryFailed => "delivery_failed",
            Self::DeliveryCleanupFailed => "delivery_cleanup_failed",
            Self::WorkloadRefused => "workload_refused",
            Self::OrgMismatch => "org_mismatch",
            Self::PublicKeyInvalid => "public_key_invalid",
            Self::NatsAccountMissing => "nats_account_missing",
            Self::SigningFailed => "signing_failed",
            Self::Unavailable => "unavailable",
            Self::TimedOut => "timed_out",
        }
    }

    pub fn password(error: &PasswordError) -> Self {
        match (error.cause(), error.reason()) {
            (Some(RefusalCause::Expired), _) => Self::TokenExpired,
            (Some(RefusalCause::Consumed), _) => Self::TokenConsumed,
            (Some(RefusalCause::UnknownSecret), Some("malformed")) => Self::TokenMalformed,
            (Some(RefusalCause::UnknownSecret), _) => Self::TokenUnknown,
            (Some(RefusalCause::NoSuchAccount), Some("address-changed")) => Self::AddressChanged,
            (Some(RefusalCause::NoSuchAccount), Some("no-password")) => Self::CredentialMissing,
            (Some(RefusalCause::NoSuchAccount), Some("inactive")) => Self::PrincipalInactive,
            (Some(RefusalCause::NoSuchAccount), _) => Self::PrincipalUnknown,
            (Some(RefusalCause::AlreadyEnrolled), _) => Self::AlreadyEnrolled,
            (Some(RefusalCause::PasswordRule), _) => Self::PasswordPolicyRefused,
            (None, Some("stored-credential")) => Self::CredentialCorrupt,
            (None, _) if error.error_type() == PasswordErrorType::Busy => Self::PasswordWorkBusy,
            (None, _) => Self::Unavailable,
        }
    }
}

/// The effective address and its provenance. A forwarded address retains the socket peer.
#[derive(Clone, Copy, Debug)]
pub enum ClientAddress {
    Socket(IpAddr),
    Forwarded { address: IpAddr, peer: IpAddr },
}

impl ClientAddress {
    pub fn ip(self) -> IpAddr {
        match self {
            Self::Socket(ip) | Self::Forwarded { address: ip, .. } => ip,
        }
    }
}

/// Evidence captured as identity decides one request.
pub struct Event {
    pub event_type: EventType,
    pub outcome: Option<Outcome>,
    pub principal_id: Option<PrincipalId>,
    pub org: Option<OrgId>,
    pub login_id: Option<Uuid>,
    pub workload: Option<String>,
    address: ClientAddress,
    attempted_email: Option<String>,
}

impl fmt::Debug for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Event")
            .field("event_type", &self.event_type)
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}

impl Event {
    pub fn new(event_type: EventType, address: ClientAddress) -> Self {
        Self {
            event_type,
            outcome: Some(Outcome::Unavailable),
            principal_id: None,
            org: None,
            login_id: None,
            workload: None,
            address,
            attempted_email: None,
        }
    }

    pub fn client_ip(&self) -> IpAddr {
        self.address.ip()
    }

    pub fn attempted_email(&mut self, email: &str) {
        // PostgreSQL text also refuses NUL. Preserve its place in the submitted address.
        self.attempted_email = Some(
            email
                .chars()
                .take(512)
                .map(|ch| if ch == '\0' { '\u{fffd}' } else { ch })
                .collect(),
        );
    }

    /// Transfer the evidence to a writer that owns the request's only append.
    pub fn take_for_append(&mut self) -> Self {
        let mut handled = Self::new(self.event_type, self.address);
        handled.outcome = None;
        std::mem::replace(self, handled)
    }

    /// Append without returning a generated value. The caller owns the transaction.
    pub async fn append(
        &self,
        connection: &mut PgConnection,
        outcome: Outcome,
    ) -> Result<(), sqlx::Error> {
        self.append_with_session(connection, outcome, None).await
    }

    /// Append in a separate transaction with a database statement deadline.
    pub async fn append_standalone(
        &self,
        connection: &mut PgConnection,
        outcome: Outcome,
    ) -> Result<(), sqlx::Error> {
        let mut tx = connection.begin().await?;
        sqlx::raw_sql("SET LOCAL statement_timeout = '750ms'")
            .execute(&mut *tx)
            .await?;
        self.append(&mut tx, outcome).await?;
        tx.commit().await
    }

    pub(crate) async fn append_with_session(
        &self,
        connection: &mut PgConnection,
        outcome: Outcome,
        jti: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let (source, peer) = match self.address {
            ClientAddress::Socket(_) => ("socket", None),
            ClientAddress::Forwarded { peer, .. } => ("forwarded", Some(peer.to_string())),
        };
        sqlx::query(
            "INSERT INTO identity.authentication_event
             (event_type, outcome, principal_id, attempted_email, ip_address, ip_source,
              peer_ip_address, org, login_id, session_jti, workload)
             VALUES ($1, $2, $3, $4, $5::text::inet, $6, $7::text::inet, $8, $9, $10, $11)",
        )
        .bind(self.event_type.code())
        .bind(outcome.code())
        .bind(self.principal_id.map(Uuid::from))
        .bind(self.attempted_email.as_deref())
        .bind(self.address.ip().to_string())
        .bind(source)
        .bind(peer)
        .bind(self.org.as_ref().map(OrgId::as_str))
        .bind(self.login_id)
        .bind(jti)
        .bind(self.workload.as_deref())
        .execute(connection)
        .await?;
        Ok(())
    }
}

/// Commit the change and its event together. A cancelled commit may finish on the
/// server, so the request cannot schedule another append while it is in flight.
pub(crate) async fn commit_success(
    mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
    event: Option<&mut Event>,
    outcome: Outcome,
    jti: Option<&str>,
) -> Result<(), sqlx::Error> {
    if let Some(event) = event {
        event.append_with_session(&mut tx, outcome, jti).await?;
        event.outcome = None;
        tx.commit().await.inspect_err(|error| {
            if error.as_database_error().is_some() {
                event.outcome = Some(Outcome::Unavailable);
            }
        })
    } else {
        tx.commit().await
    }
}
