//! Applied environment configuration. Helm owns release selections and package membership.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Triple;

/// The non-secret configuration of one project deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentConfiguration {
    pub org: String,
    pub project: String,
    pub env: String,
    pub target: String,
    pub policy: String,
    pub revocation_seconds: u64,
    #[serde(default)]
    pub schema: Schema,
    pub applications: BTreeMap<String, ApplicationConfiguration>,
    #[serde(default)]
    pub integrations: BTreeMap<String, Integration>,
}

/// Configuration shared by all releases of an application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationConfiguration {
    pub hostname: String,
    pub revocation_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Client>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, String>,
}

/// Schema behavior; an omitted mode follows the stored disposable designation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SchemaMode>,
}

/// The schema lifecycle mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SchemaMode {
    Reset,
    Migrate,
}

/// A package and its immutable web artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub package: String,
    pub web: String,
}

/// A published project integration version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integration {
    pub version: i32,
}

/// Read the applied configuration by coordinate. An unconfigured deployment returns none.
///
/// # Errors
/// Returns database errors and refuses documents with an obsolete or invalid shape.
pub async fn read(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
) -> Result<Option<EnvironmentConfiguration>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<sqlx::types::Json<EnvironmentConfiguration>>>(
        "SELECT configuration FROM registry.environment_projects WHERE org=$1 AND env=$2 AND project=$3",
    )
    .bind(&triple.org)
    .bind(triple.env.as_str())
    .bind(&triple.project)
    .fetch_optional(connection)
    .await
    .map(|value| value.flatten().map(|value| value.0))
}

/// Replace configuration of an existing deployment. The caller owns the lifecycle lock.
///
/// # Errors
/// Returns database errors. A missing deployment returns false.
pub async fn write(
    connection: &mut sqlx::PgConnection,
    configuration: &EnvironmentConfiguration,
) -> Result<bool, sqlx::Error> {
    sqlx::query(
        "UPDATE registry.environment_projects SET configuration=$4 WHERE org=$1 AND env=$2 AND project=$3",
    )
    .bind(&configuration.org)
    .bind(&configuration.env)
    .bind(&configuration.project)
    .bind(sqlx::types::Json(configuration))
    .execute(connection)
    .await
    .map(|result| result.rows_affected() == 1)
}

/// Store a restored client without replacing another application's configuration.
/// The caller holds the application lifecycle lock and binds the provisioning actor.
///
/// # Errors
/// Returns database errors. A missing application returns false.
pub async fn write_client(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
    application: &str,
    client: Option<&Client>,
) -> Result<bool, sqlx::Error> {
    sqlx::query(
        "UPDATE registry.environment_projects SET configuration=jsonb_set(configuration, ARRAY['applications',$4,'client'], $5) \
         WHERE org=$1 AND env=$2 AND project=$3 AND configuration->'applications' ? $4",
    )
    .bind(&triple.org)
    .bind(triple.env.as_str())
    .bind(&triple.project)
    .bind(application)
    .bind(sqlx::types::Json(client))
    .execute(connection)
    .await
    .map(|result| result.rows_affected() == 1)
}
