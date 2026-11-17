//! Connection rows from project databases and type metadata from the system database.

use serde_json::Value;
use sqlx::{PgConnection, Row as _};
use std::collections::BTreeMap;
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

/// Read current project authority and its system-owned type, target and principal.
///
/// # Errors
/// Returns database errors.
pub async fn fact(
    project_database: &mut PgConnection,
    system: &mut PgConnection,
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
    id: &Uuid,
) -> Result<Option<ConnectionFact>, sqlx::Error> {
    let Some(row)=sqlx::query("SELECT c.active_generation,c.status='active' AS active,g.credential_id,c.target_id,c.resource_key,c.service_principal_id,c.integration_type FROM platform.connection c JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation) WHERE c.id=$1").bind(id).fetch_optional(project_database).await? else{return Ok(None)};
    let metadata=sqlx::query("SELECT t.exclusive,target.type,p.status='active' AS active FROM registry.integration_type t CROSS JOIN registry.target target JOIN identity.principals p ON p.id=$3 JOIN registry.connection_principal link ON link.principal_id=p.id AND link.org=$4 AND link.project=$5 AND link.env=$6 WHERE t.code=$1 AND target.id=$2")
        .bind(row.try_get::<String,_>("integration_type")?).bind(row.try_get::<Uuid,_>("target_id")?).bind(row.try_get::<Uuid,_>("service_principal_id")?).bind(org).bind(project).bind(env).fetch_optional(system).await?;
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    Ok(Some(ConnectionFact {
        active_generation: row.try_get("active_generation")?,
        active: row.try_get::<bool, _>("active")? && metadata.try_get::<bool, _>("active")?,
        credential_id: row.try_get("credential_id")?,
        exclusive: metadata.try_get("exclusive")?,
        target_id: row.try_get("target_id")?,
        resource_key: row.try_get("resource_key")?,
        target_type: metadata.try_get("type")?,
        service_principal_id: row.try_get("service_principal_id")?,
    }))
}

/// Read immutable generation data locally and its runtime type from the system pool.
///
/// # Errors
/// Returns database errors.
pub async fn generation(
    project: &mut PgConnection,
    system: &mut PgConnection,
    id: &Uuid,
    generation: i64,
) -> Result<Option<ConnectionGeneration>, sqlx::Error> {
    let Some(row)=sqlx::query("SELECT c.name,c.integration_type,c.auth_type,c.external_system_id::text AS external_system,g.parameters FROM platform.connection c JOIN platform.connection_generation g ON g.connection_id=c.id WHERE c.id=$1 AND g.generation=$2").bind(id).bind(generation).fetch_optional(project).await? else{return Ok(None)};
    let integration_type =
        row.try_get::<crate::integration_code::IntegrationCode, _>("integration_type")?;
    let runtime_type =
        sqlx::query_scalar("SELECT runtime_type FROM registry.integration_type WHERE code=$1")
            .bind(&integration_type)
            .fetch_one(system)
            .await?;
    Ok(Some(ConnectionGeneration {
        name: row.try_get("name")?,
        integration_type,
        runtime_type,
        auth_type: row.try_get("auth_type")?,
        external_system: row.try_get("external_system")?,
        parameters: row.try_get("parameters")?,
    }))
}

/// Read an immutable secret reference in the selected project database.
///
/// # Errors
/// Returns database errors.
pub async fn credential(
    project: &mut PgConnection,
    id: &Uuid,
) -> Result<Option<CredentialReference>, sqlx::Error> {
    Ok(sqlx::query_as::<_, (String, String)>(
        "SELECT ref,version FROM platform.credential WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(project)
    .await?
    .map(|(reference, version)| CredentialReference { reference, version }))
}

/// Read local connection names and identities.
///
/// # Errors
/// Returns database errors.
pub async fn environment(
    project: &mut PgConnection,
) -> Result<BTreeMap<String, Uuid>, sqlx::Error> {
    Ok(
        sqlx::query_as::<_, (String, Uuid)>(
            "SELECT name,id FROM platform.connection ORDER BY name",
        )
        .fetch_all(project)
        .await?
        .into_iter()
        .collect(),
    )
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
        "SELECT version,digest FROM registry.integration_type_version WHERE integration_type=$1 AND ($2::integer IS NULL OR version=$2) ORDER BY version DESC LIMIT 1",
    )
    .bind(integration_type)
    .bind(version)
    .fetch_optional(connection)
    .await?
    .map(|(version, digest)| IntegrationVersion { version, digest }))
}
