//! Request routing and bounded writes for authentication decisions.

use std::sync::Arc;
use std::time::Duration;

use sqlx::{Connection as _, PgConnection};
use wamn_identity::authentication_event::{Event, EventType, Outcome};
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
            return write(connection, event, outcome).await;
        }
        let mut connection = inner.pool.acquire().await?;
        write(&mut connection, event, outcome).await
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

async fn write(
    connection: &mut PgConnection,
    event: &Event,
    outcome: Outcome,
) -> Result<(), sqlx::Error> {
    let mut tx = connection.begin().await?;
    sqlx::raw_sql("SET LOCAL statement_timeout = '750ms'")
        .execute(&mut *tx)
        .await?;
    event.append(&mut tx, outcome).await?;
    tx.commit().await
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
