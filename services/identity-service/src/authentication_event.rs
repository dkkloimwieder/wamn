//! Request routing and bounded writes for authentication decisions.

use std::sync::Arc;
use std::time::Duration;

use sqlx::PgConnection;
use wamn_identity::authentication_event::{Event, EventType, Outcome};
use wamn_identity::identity::issuer::Issuer;
use wamn_identity::password::Invitation;
use wamn_identity::password_requests;
use wamn_identity_protocol::nats::{SIGN_HOST_USER_PATH, SIGN_USER_PATH};

use crate::Inner;

const APPEND_TIMEOUT: Duration = Duration::from_secs(1);
pub(super) const PENDING_APPENDS: usize = 8;

pub(super) fn password_event(path: &str) -> Option<EventType> {
    Some(match path {
        "/password/session" => EventType::PasswordSignIn,
        "/password/renew" => EventType::SessionRenew,
        "/password/logout" => EventType::SessionSignOut,
        "/password/logout-all" => EventType::SessionSignOutAll,
        "/password/recover" => EventType::PasswordRecoveryRequest,
        "/password/reset" => EventType::PasswordReset,
        "/password/enroll" => EventType::PasswordEnroll,
        _ => return None,
    })
}

pub(super) fn workload_event(path: &str) -> Option<EventType> {
    Some(match path {
        "/invitations" => EventType::InvitationRequest,
        SIGN_USER_PATH => EventType::NatsWorkloadCredentialIssue,
        SIGN_HOST_USER_PATH => EventType::NatsHostCredentialIssue,
        _ => return None,
    })
}

pub(super) async fn append(
    inner: &Inner,
    connection: Option<&mut PgConnection>,
    event: &Event,
    outcome: Outcome,
) -> bool {
    let result = tokio::time::timeout(APPEND_TIMEOUT, async {
        if let Some(connection) = connection {
            return event.append_standalone(connection, outcome).await;
        }
        let mut connection = inner.pool.acquire().await?;
        event.append_standalone(&mut connection, outcome).await
    })
    .await;
    if matches!(result, Ok(Ok(()))) {
        return true;
    }
    tracing::warn!(
        event_type = event.event_type.code(),
        "authentication event append failed"
    );
    false
}

pub(super) async fn finish_delivery(
    mut connection: impl AsMut<PgConnection> + Send + 'static,
    token: Invitation,
    accepted: bool,
    event: &mut Event,
) -> bool {
    let mut event = event.take_for_append();
    // Once mail returns, complete the bounded writes even if the request deadline elapses.
    tokio::spawn(async move {
        let mut issuer = Issuer(connection.as_mut());
        match password_requests::deliver(&mut issuer, &token, accepted, &mut event).await {
            Ok(outcome) => outcome == Outcome::DeliveryAccepted,
            Err(failure) => {
                tracing::warn!(
                    event_type = event.event_type.code(),
                    cleanup_failed = failure.cleanup_failed,
                    "authentication delivery event append failed"
                );
                false
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// Keep best-effort appends off the request's reply path and bound concurrent work.
pub(super) fn finish(inner: &Arc<Inner>, event: Event) {
    let Some(outcome) = event.outcome else {
        return;
    };
    let Ok(slot) = Arc::clone(&inner.event_slots).try_acquire_owned() else {
        tracing::warn!(
            event_type = event.event_type.code(),
            "authentication event capacity exhausted"
        );
        return;
    };
    let inner = Arc::clone(inner);
    tokio::spawn(async move {
        let _slot = slot;
        let _ = append(&inner, None, &event, outcome).await;
    });
}
