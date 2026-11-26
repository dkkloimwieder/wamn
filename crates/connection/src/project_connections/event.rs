//! Project connection history. Event writers accept only fixed outcome codes.

use std::time::Duration;

use sqlx::{Connection as _, PgConnection, Postgres, Transaction};
use uuid::Uuid;
use wamn_identity_protocol::PrincipalId;

#[derive(Debug, sqlx::FromRow)]
pub struct EventRow {
    pub id: String,
    pub generation: i64,
    pub principal_id: String,
    pub occurred_at: String,
    pub event_type: String,
    pub outcome: String,
}

#[derive(Debug, Clone, Copy)]
pub enum EventType {
    Activate,
    Deactivate,
    CredentialChange,
    OAuthAuthorize,
    OAuthCallback,
    TokenRefresh,
}

impl EventType {
    fn code(self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::Deactivate => "deactivate",
            Self::CredentialChange => "credential_change",
            Self::OAuthAuthorize => "oauth_authorize",
            Self::OAuthCallback => "oauth_callback",
            Self::TokenRefresh => "token_refresh",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum EventOutcome {
    Succeeded,
    Refused,
    Failed,
    Superseded,
}

impl EventOutcome {
    fn code(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Refused => "refused",
            Self::Failed => "failed",
            Self::Superseded => "superseded",
        }
    }
}

#[derive(Debug)]
pub struct ConnectionEvent {
    pub connection: Uuid,
    pub generation: i64,
    pub event_type: EventType,
    /// When absent, use the identity bound to the writer's transaction.
    pub principal: Option<PrincipalId>,
}

impl ConnectionEvent {
    pub async fn append(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        outcome: EventOutcome,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO platform.connection_event
                 (connection_id, generation, event_type, outcome, principal_id)
             VALUES ($1, $2, $3, $4, coalesce($5, wamn_authority.current_principal_id()))",
        )
        .bind(self.connection)
        .bind(self.generation)
        .bind(self.event_type.code())
        .bind(outcome.code())
        .bind(self.principal.map(Uuid::from))
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Record a refusal after its transaction ends, keeping the original reply.
    pub async fn record_refusal(&self, database: &mut PgConnection, outcome: EventOutcome) {
        let result = tokio::time::timeout(Duration::from_secs(1), async {
            let mut tx = database.begin().await?;
            sqlx::raw_sql("SET LOCAL statement_timeout = '750ms'")
                .execute(&mut *tx)
                .await?;
            self.append(&mut tx, outcome).await?;
            tx.commit().await
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            tracing::warn!(
                event_type = self.event_type.code(),
                outcome = outcome.code(),
                "connection event append failed"
            );
        }
    }
}
