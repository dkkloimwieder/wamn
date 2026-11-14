//! Typed applied settings and desired releases. Helm owns deployment history.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::{Connection as _, Row as _};

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
    /// Desired release. Deployment documents cannot supply this field.
    #[serde(skip)]
    pub release: Option<String>,
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

/// Read applied settings and desired releases. An unconfigured deployment returns none.
///
/// # Errors
/// Returns database errors.
pub async fn read(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
) -> Result<Option<EnvironmentConfiguration>, sqlx::Error> {
    // One statement gives every child the same snapshot, including concurrent client writes.
    let rows = sqlx::query(
        "SELECT t.name AS target, e.policy_name, p.revocation_seconds::text AS project_seconds, p.schema_mode, c.* \
         FROM registry.environment_projects p JOIN registry.environments e USING (org,env) \
         JOIN registry.target t ON t.id=e.target_id LEFT JOIN LATERAL ( \
           SELECT 1 AS record_type, application AS name, hostname AS value, revocation_seconds::text AS seconds, \
             client_package, client_web, release_digest, NULL::text AS alias, NULL::integer AS version \
           FROM registry.environment_applications WHERE org=p.org AND env=p.env AND project=p.project \
           UNION ALL SELECT 2, application, connection_name, NULL, NULL, NULL, NULL, alias, NULL \
           FROM registry.environment_application_connections WHERE org=p.org AND env=p.env AND project=p.project \
           UNION ALL SELECT 3, name, NULL, NULL, NULL, NULL, NULL, NULL, version \
           FROM registry.environment_integrations WHERE org=p.org AND env=p.env AND project=p.project \
         ) c ON true WHERE p.org=$1 AND p.env=$2 AND p.project=$3 AND p.revocation_seconds IS NOT NULL \
         ORDER BY c.record_type",
    )
    .bind(&triple.org).bind(triple.env.as_str()).bind(&triple.project)
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
        env: triple.env.to_string(),
        project: triple.project.clone(),
        target: row.try_get("target")?,
        policy: row.try_get("policy_name")?,
        revocation_seconds: seconds(row, "project_seconds")?,
        schema: Schema { mode },
        applications: BTreeMap::new(),
        integrations: BTreeMap::new(),
    };
    for row in rows {
        match row.try_get::<Option<i32>, _>("record_type")? {
            Some(1) => {
                let package: Option<String> = row.try_get("client_package")?;
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
                        hostname: row.try_get("value")?,
                        revocation_seconds: seconds(&row, "seconds")?,
                        client,
                        release: row.try_get("release_digest")?,
                        connections: BTreeMap::new(),
                    },
                );
            }
            Some(2) => {
                let application: &str = row.try_get("name")?;
                configuration
                    .applications
                    .get_mut(application)
                    .expect("connection references a stored application")
                    .connections
                    .insert(row.try_get("alias")?, row.try_get("value")?);
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

fn seconds(row: &sqlx::postgres::PgRow, column: &str) -> Result<u64, sqlx::Error> {
    row.try_get::<&str, _>(column)?
        .parse()
        .map_err(|error| sqlx::Error::ColumnDecode {
            index: column.to_owned(),
            source: Box::new(error),
        })
}

/// Refuse a policy that would move an established environment to another cluster.
/// Call before provisioning effects while holding the environment lifecycle lock.
///
/// # Errors
/// Returns database errors or an invalid argument when the policy changes placement.
pub async fn require_placement(
    connection: &mut sqlx::PgConnection,
    org: &str,
    env: &str,
    policy: &str,
) -> Result<(), sqlx::Error> {
    let preserved: Option<bool> = sqlx::query_scalar(
        "SELECT requested.name IS NOT NULL AND (o.placement_type='pooled' OR \
         COALESCE(established.recovery_domain->>'shared-with',established.name) = \
         COALESCE(requested.recovery_domain->>'shared-with',requested.name)) \
         FROM registry.environments e JOIN registry.orgs o ON o.id=e.org \
         JOIN registry.env_policies established ON established.org=e.org AND established.name=e.policy_name \
         LEFT JOIN registry.env_policies requested ON requested.org=e.org AND requested.name=$3 \
         WHERE e.org=$1 AND e.env=$2 FOR UPDATE OF e",
    )
    .bind(org)
    .bind(env)
    .bind(policy)
    .fetch_optional(connection)
    .await?;
    if preserved == Some(false) {
        return Err(sqlx::Error::InvalidArgument(format!(
            "policy {policy:?} must exist and preserve the database cluster of {org}/{env}"
        )));
    }
    Ok(())
}

/// Replace applied settings atomically, preserving each surviving application's release.
/// The caller owns the environment lifecycle lock and binds the provisioning actor.
///
/// # Errors
/// Returns database errors or refuses a policy that changes established placement.
/// A missing deployment returns false.
pub async fn write(
    connection: &mut sqlx::PgConnection,
    configuration: &EnvironmentConfiguration,
) -> Result<bool, sqlx::Error> {
    let mut transaction = connection.begin().await?;
    let c = configuration;
    require_placement(&mut transaction, &c.org, &c.env, &c.policy).await?;
    let mode = c.schema.mode.map(|mode| match mode {
        SchemaMode::Reset => "reset",
        SchemaMode::Migrate => "migrate",
    });
    let updated = sqlx::query(
        "UPDATE registry.environment_projects SET revocation_seconds=$4::text::numeric, schema_mode=$5 \
         WHERE org=$1 AND env=$2 AND project=$3",
    )
    .bind(&c.org).bind(&c.env).bind(&c.project).bind(c.revocation_seconds.to_string()).bind(mode)
    .execute(&mut *transaction).await?;
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
    let applications: Vec<&str> = c.applications.keys().map(String::as_str).collect();
    sqlx::query("DELETE FROM registry.environment_applications WHERE org=$1 AND env=$2 AND project=$3 AND NOT(application=ANY($4))")
        .bind(&c.org).bind(&c.env).bind(&c.project).bind(&applications).execute(&mut *transaction).await?;
    sqlx::query("DELETE FROM registry.environment_application_connections WHERE org=$1 AND env=$2 AND project=$3")
        .bind(&c.org).bind(&c.env).bind(&c.project).execute(&mut *transaction).await?;
    for (name, application) in &c.applications {
        sqlx::query(
            "INSERT INTO registry.environment_applications \
             (org,env,project,application,hostname,revocation_seconds,client_package,client_web) \
             VALUES ($1,$2,$3,$4,$5,$6::text::numeric,$7,$8) \
             ON CONFLICT (org,env,project,application) DO UPDATE SET hostname=EXCLUDED.hostname, \
             revocation_seconds=EXCLUDED.revocation_seconds, client_package=EXCLUDED.client_package, client_web=EXCLUDED.client_web",
        )
        .bind(&c.org).bind(&c.env).bind(&c.project).bind(name).bind(&application.hostname)
        .bind(application.revocation_seconds.to_string())
        .bind(application.client.as_ref().map(|client| &client.package))
        .bind(application.client.as_ref().map(|client| &client.web))
        .execute(&mut *transaction).await?;
        for (alias, connection_name) in &application.connections {
            sqlx::query(
                "INSERT INTO registry.environment_application_connections (org,env,project,application,alias,connection_name) \
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(&c.org).bind(&c.env).bind(&c.project).bind(name)
            .bind(alias).bind(connection_name).execute(&mut *transaction).await?;
        }
    }
    sqlx::query(
        "DELETE FROM registry.environment_integrations WHERE org=$1 AND env=$2 AND project=$3",
    )
    .bind(&c.org)
    .bind(&c.env)
    .bind(&c.project)
    .execute(&mut *transaction)
    .await?;
    for (name, integration) in &c.integrations {
        sqlx::query("INSERT INTO registry.environment_integrations (org,env,project,name,version) VALUES ($1,$2,$3,$4,$5)")
            .bind(&c.org).bind(&c.env).bind(&c.project).bind(name).bind(integration.version).execute(&mut *transaction).await?;
    }
    transaction.commit().await?;
    Ok(true)
}

/// Record the desired release and client before writing its Helm revision.
/// The caller owns the application lifecycle lock and binds the provisioning actor.
///
/// # Errors
/// Returns database errors. A missing application returns false.
pub async fn write_selection(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
    application: &str,
    release: Option<&str>,
    client: Option<&Client>,
) -> Result<bool, sqlx::Error> {
    sqlx::query(
        "WITH parent AS (UPDATE registry.environment_projects p SET revocation_seconds=p.revocation_seconds \
         WHERE org=$1 AND env=$2 AND project=$3 AND EXISTS (SELECT FROM registry.environment_applications a \
         WHERE (a.org,a.env,a.project)=(p.org,p.env,p.project) AND a.application=$4) RETURNING org,env,project) \
         UPDATE registry.environment_applications a SET release_digest=$5, client_package=$6, client_web=$7 \
         FROM parent p WHERE (a.org,a.env,a.project)=(p.org,p.env,p.project) AND a.application=$4",
    )
    .bind(&triple.org).bind(triple.env.as_str()).bind(&triple.project).bind(application).bind(release)
    .bind(client.map(|client| &client.package)).bind(client.map(|client| &client.web))
    .execute(connection).await.map(|result| result.rows_affected() == 1)
}
