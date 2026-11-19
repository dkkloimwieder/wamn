//! Typed applied project settings and environment connection pins.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sqlx::{Connection as _, Row as _};

use wamn_registry::Triple;

/// The non-secret configuration of one project deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentConfiguration {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub env: wamn_registry::EnvName,
    pub target: String,
    pub policy: wamn_registry::EnvName,
    #[serde(default)]
    pub schema: Schema,
    pub applications: BTreeMap<crate::ApplicationName, ApplicationConfiguration>,
    #[serde(default)]
    pub integrations: BTreeMap<String, Integration>,
}

/// Configuration shared by all releases of an application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationConfiguration {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Client>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, ConnectionReference>,
}

/// A bare connection name. The version is pinned once for the environment.
#[derive(Debug, Clone, PartialEq, Eq, schemars::JsonSchema)]
pub struct ConnectionReference(#[schemars(regex(pattern = "^[a-z][a-z0-9_-]{0,62}$"))] String);

impl ConnectionReference {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn valid_connection_name(name: &str) -> bool {
    name.len() <= 63
        && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

impl fmt::Display for ConnectionReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ConnectionReference {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ConnectionReference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        if !valid_connection_name(&name) {
            return Err(serde::de::Error::custom("invalid connection name"));
        }
        Ok(Self(name))
    }
}

/// Schema behavior; an omitted mode follows the stored disposable designation.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SchemaMode>,
}

/// The schema lifecycle mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SchemaMode {
    Reset,
    Migrate,
}

/// A package and its immutable web artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Client {
    #[schemars(with = "String")]
    pub package: crate::PackageName,
    #[schemars(with = "String", regex(pattern = "^sha256:[0-9a-f]{64}$"))]
    pub web: wamn_digest::Digest,
}

/// The environment pin for a published integration type version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Integration {
    #[schemars(range(min = 1))]
    pub version: i32,
}

/// Read applied settings and environment connection pins. An unconfigured deployment returns none.
///
/// # Errors
/// Returns database errors.
pub async fn read(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
) -> Result<Option<EnvironmentConfiguration>, sqlx::Error> {
    // One statement gives every child the same snapshot, including concurrent client writes.
    let rows = sqlx::query(
        "SELECT t.name AS target, e.policy_name, p.schema_mode, c.* \
         FROM registry.environment_projects p JOIN registry.environments e USING (org,env) \
         JOIN registry.target t ON t.id=e.target_id LEFT JOIN LATERAL ( \
           SELECT 1 AS record_type, application AS name, label AS value, \
             client_package, client_web, NULL::text AS alias, NULL::integer AS version \
           FROM registry.environment_applications WHERE org=p.org AND env=p.env AND project=p.project \
           UNION ALL SELECT 2, application, connection_name, NULL, NULL, alias, NULL \
           FROM registry.environment_application_connections WHERE org=p.org AND env=p.env AND project=p.project \
           UNION ALL SELECT 3, name, NULL, NULL, NULL, NULL, version \
           FROM registry.environment_integrations WHERE org=p.org AND env=p.env \
         ) c ON true WHERE p.org=$1 AND p.env=$2 AND p.project=$3 AND p.schema_mode IS NOT NULL \
         ORDER BY c.record_type",
    )
    .bind(&triple.org).bind(&triple.env).bind(&triple.project)
    .fetch_all(connection).await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let mode = match row.try_get::<Option<&str>, _>("schema_mode")? {
        Some("reset") => Some(SchemaMode::Reset),
        Some("migrate") => Some(SchemaMode::Migrate),
        None => None,
        Some(_) => return Err(sqlx::Error::Decode("invalid stored schema mode".into())),
    };
    let mut configuration = EnvironmentConfiguration {
        org: triple.org.clone(),
        env: triple.env.clone(),
        project: triple.project.clone(),
        target: row.try_get("target")?,
        policy: row.try_get("policy_name")?,
        schema: Schema { mode },
        applications: BTreeMap::new(),
        integrations: BTreeMap::new(),
    };
    for row in rows {
        match row.try_get::<Option<i32>, _>("record_type")? {
            Some(1) => {
                let package: Option<crate::PackageName> = row.try_get("client_package")?;
                let client = package
                    .map(|package| {
                        Ok::<_, sqlx::Error>(Client {
                            package,
                            web: row.try_get("client_web")?,
                        })
                    })
                    .transpose()?;
                configuration.applications.insert(
                    row.try_get("name")?,
                    ApplicationConfiguration {
                        label: row.try_get("value")?,
                        client,
                        connections: BTreeMap::new(),
                    },
                );
            }
            Some(2) => {
                let application: crate::ApplicationName = row.try_get("name")?;
                configuration
                    .applications
                    .get_mut(&application)
                    .expect("connection references a stored application")
                    .connections
                    .insert(
                        row.try_get("alias")?,
                        ConnectionReference(row.try_get("value")?),
                    );
            }
            Some(3) => {
                configuration.integrations.insert(
                    row.try_get("name")?,
                    Integration {
                        version: row.try_get("version")?,
                    },
                );
            }
            _ => {}
        }
    }
    Ok(Some(configuration))
}

/// Replace one project's settings atomically and upsert the environment pins.
/// The caller owns the environment lifecycle lock, checks placement, and binds the provisioning actor.
///
/// # Errors
/// Returns database errors.
/// A missing deployment returns false.
pub async fn write(
    connection: &mut sqlx::PgConnection,
    configuration: &EnvironmentConfiguration,
) -> Result<bool, sqlx::Error> {
    let mut transaction = connection.begin().await?;
    let c = configuration;
    let mode = c.schema.mode.map(|mode| match mode {
        SchemaMode::Reset => "reset",
        SchemaMode::Migrate => "migrate",
    });
    let updated = sqlx::query(
        "UPDATE registry.environment_projects p SET schema_mode=COALESCE($4, \
         (SELECT CASE WHEN e.disposable THEN 'reset' ELSE 'migrate' END \
          FROM registry.environments e WHERE e.org=p.org AND e.env=p.env)) \
         WHERE org=$1 AND env=$2 AND project=$3",
    )
    .bind(&c.org)
    .bind(&c.env)
    .bind(&c.project)
    .bind(mode)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() == 0 {
        transaction.rollback().await?;
        return Ok(false);
    }
    // Policy and target already have one typed owner at the shared environment.
    let target: String = sqlx::query_scalar("SELECT id::text FROM registry.target WHERE name=$1")
        .bind(&c.target)
        .fetch_one(&mut *transaction)
        .await?;
    sqlx::query("UPDATE registry.environments SET policy_name=$3, target_id=$4::text::uuid WHERE org=$1 AND env=$2")
        .bind(&c.org).bind(&c.env).bind(&c.policy).bind(target).execute(&mut *transaction).await?;
    for (name, integration) in &c.integrations {
        sqlx::query("INSERT INTO registry.environment_integrations (org,env,name,version) VALUES ($1,$2,$3,$4) \
                     ON CONFLICT (org,env,name) DO UPDATE SET version=EXCLUDED.version")
            .bind(&c.org).bind(&c.env).bind(name).bind(integration.version).execute(&mut *transaction).await?;
    }
    let applications: Vec<&crate::ApplicationName> = c.applications.keys().collect();
    sqlx::query("DELETE FROM registry.environment_applications WHERE org=$1 AND env=$2 AND project=$3 AND NOT(application=ANY($4))")
        .bind(&c.org).bind(&c.env).bind(&c.project).bind(&applications).execute(&mut *transaction).await?;
    sqlx::query("DELETE FROM registry.environment_application_connections WHERE org=$1 AND env=$2 AND project=$3")
        .bind(&c.org).bind(&c.env).bind(&c.project).execute(&mut *transaction).await?;
    for (name, application) in &c.applications {
        sqlx::query(
            "INSERT INTO registry.environment_applications \
             (org,env,project,application,label,client_package,client_web) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (org,env,project,application) DO UPDATE SET label=EXCLUDED.label, \
             client_package=EXCLUDED.client_package, client_web=EXCLUDED.client_web",
        )
        .bind(&c.org).bind(&c.env).bind(&c.project).bind(name).bind(&application.label)
        .bind(application.client.as_ref().map(|client| &client.package))
        .bind(application.client.as_ref().map(|client| &client.web))
        .execute(&mut *transaction).await?;
        for (alias, connection) in &application.connections {
            sqlx::query(
                "INSERT INTO registry.environment_application_connections (org,env,project,application,alias,connection_name) \
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(&c.org).bind(&c.env).bind(&c.project).bind(name)
            .bind(alias).bind(connection.as_str()).execute(&mut *transaction).await?;
        }
    }
    transaction.commit().await?;
    Ok(true)
}

/// Remove stale pins after all of an environment's project binding writes succeed.
pub async fn retain_integrations(
    connection: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    env: &wamn_registry::EnvName,
    names: &[&str],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM registry.environment_integrations WHERE org=$1 AND env=$2 AND NOT(name=ANY($3))")
        .bind(org).bind(env).bind(names).execute(connection).await?;
    Ok(())
}
