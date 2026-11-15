//! Typed connection facts and credential references from the system database.

use std::collections::BTreeMap;

use serde_json::Value;
use sqlx::{PgConnection, Row as _};
use uuid::Uuid;

/// Mutable authority and lease coordinates of one environment connection.
#[derive(Debug)]
pub struct ConnectionFact {
    pub active_generation: i64,
    pub active: bool,
    pub credential_id: Option<Uuid>,
    pub exclusive: bool,
    pub target_id: Uuid,
    pub resource_key: Option<String>,
    pub target_type: String,
    pub service_principal_id: wamn_identity_protocol::PrincipalId,
}

/// One generation of a connection, apart from its secret.
#[derive(Clone, Debug)]
pub struct ConnectionGeneration {
    pub name: String,
    pub integration_type: crate::integration_code::IntegrationCode,
    pub runtime_type: String,
    pub auth_type: String,
    pub external_system: Option<String>,
    pub parameters: Value,
}

/// One published version of an integration type.
#[derive(Clone, Debug)]
pub struct IntegrationVersion {
    pub version: i32,
    pub digest: Option<wamn_digest::Digest>,
}

/// A reference to credential material in the configured secret store.
#[derive(Debug)]
pub struct CredentialReference {
    pub reference: String,
    pub version: String,
}

/// Read one connection's current authority through the authenticated reader login.
///
/// # Errors
/// Returns database errors.
pub async fn fact(
    connection: &mut PgConnection,
    org: &str,
    project: &str,
    env: &crate::EnvName,
    id: &Uuid,
) -> Result<Option<ConnectionFact>, sqlx::Error> {
    sqlx::query("SELECT active_generation, active, credential_id, exclusive, target_id, resource_key, target_type, service_principal_id FROM registry.read_connection_fact($1,$2,$3,$4)")
        .bind(org).bind(project).bind(env).bind(id).fetch_optional(connection).await?
        .map(|row| Ok(ConnectionFact {
            active_generation: row.try_get(0)?, active: row.try_get(1)?, credential_id: row.try_get(2)?,
            exclusive: row.try_get(3)?, target_id: row.try_get(4)?, resource_key: row.try_get(5)?,
            target_type: row.try_get(6)?, service_principal_id: row.try_get(7)?,
        })).transpose()
}

/// Read an immutable connection generation through the authenticated reader login.
///
/// # Errors
/// Returns database errors.
pub async fn generation(
    connection: &mut PgConnection,
    org: &str,
    project: &str,
    env: &crate::EnvName,
    id: &Uuid,
    generation: i64,
) -> Result<Option<ConnectionGeneration>, sqlx::Error> {
    sqlx::query("SELECT name, integration_type, runtime_type, auth_type, external_system, parameters FROM registry.read_connection_generation($1,$2,$3,$4,$5)")
        .bind(org).bind(project).bind(env).bind(id).bind(generation).fetch_optional(connection).await?
        .map(|row| Ok(ConnectionGeneration {
            name: row.try_get(0)?, integration_type: row.try_get(1)?, runtime_type: row.try_get(2)?,
            auth_type: row.try_get(3)?, external_system: row.try_get(4)?, parameters: row.try_get(5)?,
        })).transpose()
}

/// Read an admitted credential's immutable secret reference.
///
/// # Errors
/// Returns database errors.
pub async fn credential(
    connection: &mut PgConnection,
    org: &str,
    project: &str,
    env: &crate::EnvName,
    id: &Uuid,
) -> Result<Option<CredentialReference>, sqlx::Error> {
    let row =
        sqlx::query("SELECT store, ref, version FROM registry.read_connection_secret($1,$2,$3,$4)")
            .bind(org)
            .bind(project)
            .bind(env)
            .bind(id)
            .fetch_optional(connection)
            .await?;
    match row {
        Some(row) if row.try_get::<Option<String>, _>(0)?.is_some() => {
            Ok(Some(CredentialReference {
                reference: row.try_get(1)?,
                version: row.try_get(2)?,
            }))
        }
        _ => Ok(None),
    }
}

/// Read the environment's active integration names and connection identities.
///
/// # Errors
/// Returns database errors.
pub async fn environment(
    connection: &mut PgConnection,
    org: &str,
    project: &str,
    env: &crate::EnvName,
) -> Result<BTreeMap<String, Uuid>, sqlx::Error> {
    Ok(sqlx::query_as::<_, (String, Uuid)>(
        "SELECT name, id FROM registry.read_env_connections($1,$2,$3)",
    )
    .bind(org)
    .bind(project)
    .bind(env)
    .fetch_all(connection)
    .await?
    .into_iter()
    .collect())
}

/// Read a published integration version, or its newest version when omitted.
///
/// # Errors
/// Returns database errors.
pub async fn integration_version(
    connection: &mut PgConnection,
    integration_type: &crate::integration_code::IntegrationCode,
    version: Option<i32>,
) -> Result<Option<IntegrationVersion>, sqlx::Error> {
    Ok(sqlx::query_as::<_, (i32, Option<wamn_digest::Digest>)>(
        "SELECT version, digest FROM registry.read_integration_version($1,$2)",
    )
    .bind(integration_type)
    .bind(version)
    .fetch_optional(connection)
    .await?
    .map(|(version, digest)| IntegrationVersion { version, digest }))
}

/// Read the project and environment projection as the registry's JSON document.
///
/// # Errors
/// Returns database errors.
pub async fn projection(
    connection: &mut PgConnection,
    org: &str,
    project: &str,
    env: &crate::EnvName,
) -> Result<Value, sqlx::Error> {
    sqlx::query_scalar("SELECT registry.connection_projection($1,$2,$3)")
        .bind(org)
        .bind(project)
        .bind(env)
        .fetch_one(connection)
        .await
}
