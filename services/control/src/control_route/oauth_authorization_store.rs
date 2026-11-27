//! Transaction-scoped storage for encrypted OAuth authorization state.

use serde_json::Value;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;
use wamn_envelope::encryption::Encrypted;
use wamn_identity_protocol::PrincipalId;

/// Authorization state read under a row lock for a single callback.
#[derive(Debug)]
pub(super) struct LockedAuthorization {
    pub connection: Uuid,
    pub generation: i64,
    pub verifier: Encrypted,
    pub principal: PrincipalId,
    pub unexpired: bool,
}

#[derive(FromRow)]
struct AuthorizationRow {
    connection_id: Uuid,
    generation: i64,
    code_verifier: Vec<u8>,
    wrapped_key: Vec<u8>,
    nonce: Vec<u8>,
    key_id: String,
    created_by: Uuid,
    unexpired: bool,
}

/// Store the encrypted verifier with the current connection generation for ten minutes.
///
/// # Errors
/// Returns an error if the authorization state cannot be stored.
pub(super) async fn insert(
    connection: &mut PgConnection,
    state: &str,
    connection_id: Uuid,
    generation: i64,
    verifier: &Encrypted,
    caller: PrincipalId,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO platform.oauth_authorization(state,connection_id,generation,code_verifier,wrapped_key,nonce,key_id,created_by,expires_at) \
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp()+interval '10 minutes')")
        .bind(state).bind(connection_id).bind(generation).bind(&verifier.ciphertext)
        .bind(&verifier.wrapped_key).bind(&verifier.nonce).bind(&verifier.key_id)
        .bind(Uuid::from(caller)).execute(connection).await?;
    Ok(())
}

/// Lock the authorization row for this callback transaction, if it exists.
///
/// # Errors
/// Returns an error if the row cannot be locked or decoded.
pub(super) async fn lock(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    state: &str,
) -> Result<Option<LockedAuthorization>, sqlx::Error> {
    let row = sqlx::query_as::<_, AuthorizationRow>(
        "SELECT connection_id,generation,code_verifier,wrapped_key,nonce,key_id,created_by,expires_at>clock_timestamp() AS unexpired \
         FROM platform.oauth_authorization WHERE state=$1 FOR UPDATE",
    )
    .bind(state)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(row.map(|row| LockedAuthorization {
        connection: row.connection_id,
        generation: row.generation,
        verifier: Encrypted {
            ciphertext: row.code_verifier,
            wrapped_key: row.wrapped_key,
            nonce: row.nonce,
            key_id: row.key_id,
        },
        principal: PrincipalId::from(row.created_by),
        unexpired: row.unexpired,
    }))
}

/// Lock the pending state’s live connection before locking the state itself.
///
/// # Errors
/// Returns an error if the database cannot read or lock the connection.
pub(super) async fn lock_connection(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    state: &str,
) -> Result<Option<Value>, sqlx::Error> {
    sqlx::query_scalar("SELECT to_jsonb(c)||jsonb_build_object('parameters',g.parameters,'credential',jsonb_build_object('store',k.store,'ref',k.ref,'version',k.version)) FROM platform.connection c JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation) LEFT JOIN platform.credential k ON k.id=g.credential_id WHERE c.id=(SELECT connection_id FROM platform.oauth_authorization WHERE state=$1) AND c.active FOR NO KEY UPDATE OF c")
        .bind(state).fetch_optional(&mut **transaction).await
}

/// Delete the authorization state inside the transaction that uses it.
///
/// # Errors
/// Returns an error if the state cannot be deleted.
pub(super) async fn delete(
    connection: &mut PgConnection,
    state: &str,
) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM platform.oauth_authorization WHERE state=$1")
            .bind(state)
            .execute(connection)
            .await?
            .rows_affected()
            != 0,
    )
}
