//! Find retired credential versions under the existing cleanup transaction.

use sqlx::Row as _;
use wamn_registry::connections::CredentialReference;

const RETIRED_CREDENTIALS_SQL: &str = "SELECT k.store,k.ref,k.version FROM platform.credential k JOIN platform.connection c ON c.id=k.connection_id JOIN platform.connection_generation active ON (active.connection_id,active.generation)=(c.id,c.active_generation) JOIN LATERAL (SELECT max(generation) AS generation FROM platform.connection_generation WHERE connection_id=c.id AND credential_id=k.id) last_use ON true JOIN LATERAL (SELECT created_at FROM platform.connection_generation WHERE connection_id=c.id AND generation>last_use.generation ORDER BY generation LIMIT 1) successor ON true WHERE active.credential_id IS DISTINCT FROM k.id AND successor.created_at<now()-interval '5 minutes'";

/// Read credential versions whose replacement generation has aged five minutes.
pub async fn retired_credentials(
    connection: &mut sqlx::PgConnection,
) -> Result<Vec<CredentialReference>, sqlx::Error> {
    let rows = sqlx::query(RETIRED_CREDENTIALS_SQL)
        .fetch_all(connection)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| CredentialReference {
            reference: row.get(1),
            version: row.get(2),
        })
        .collect())
}
