use sqlx::{Connection as _, Row as _};
use std::collections::BTreeMap;
use wamn_registry::{
    ApplicationName, Triple,
    configuration::{
        ApplicationConfiguration, Client, ConfigurationStore, ConnectionReference,
        EnvironmentConfiguration, EnvironmentSettings, Integration, Schema, SchemaMode,
    },
};

struct ConfigurationHeader<'a> {
    target: String,
    schema_mode: Option<&'a str>,
    readiness_budget_seconds: i32,
    drain_bound_seconds: i32,
}

impl ConfigurationStore for super::Registry<'_> {
    type Error = sqlx::Error;

    async fn read(
        &mut self,
        triple: &Triple,
    ) -> Result<Option<EnvironmentConfiguration>, sqlx::Error> {
        // One statement gives every child the same snapshot, including concurrent client writes.
        let rows = sqlx::query(
        "SELECT t.name AS target, e.readiness_budget_seconds, e.drain_bound_seconds, p.schema_mode, c.* \
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
    .bind(triple.org.as_str()).bind(triple.env.as_str()).bind(triple.project.as_str())
    .fetch_all(&mut *self.connection).await?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let header = ConfigurationHeader {
            target: row.try_get("target")?,
            schema_mode: row.try_get("schema_mode")?,
            readiness_budget_seconds: row.try_get("readiness_budget_seconds")?,
            drain_bound_seconds: row.try_get("drain_bound_seconds")?,
        };
        let mode = match header.schema_mode {
            Some("reset") => Some(SchemaMode::Reset),
            Some("migrate") => Some(SchemaMode::Migrate),
            None => None,
            Some(_) => return Err(sqlx::Error::Decode("invalid stored schema mode".into())),
        };
        let mut configuration = EnvironmentConfiguration {
            settings: EnvironmentSettings {
                org: triple.org.clone(),
                env: triple.env.clone(),
                project: triple.project.clone(),
                target: header.target,
                schema: Schema { mode },
                integrations: BTreeMap::new(),
            },
            readiness_budget_seconds: header.readiness_budget_seconds,
            drain_bound_seconds: header.drain_bound_seconds,
            applications: BTreeMap::new(),
        };
        for row in rows {
            match row.try_get::<Option<i32>, _>("record_type")? {
                Some(1) => {
                    let package = row
                        .try_get::<Option<String>, _>("client_package")?
                        .map(|value| super::decode_text(value, "client_package"))
                        .transpose()?;
                    let client = package
                        .map(|package| {
                            Ok::<_, sqlx::Error>(Client {
                                package,
                                web: row.try_get::<&str, _>("client_web")?.parse().map_err(
                                    |source| sqlx::Error::ColumnDecode {
                                        index: "client_web".into(),
                                        source: Box::new(source),
                                    },
                                )?,
                            })
                        })
                        .transpose()?;
                    configuration.applications.insert(
                        super::decode_text(row.try_get("name")?, "name")?,
                        ApplicationConfiguration {
                            label: row.try_get("value")?,
                            client,
                            connections: BTreeMap::new(),
                        },
                    );
                }
                Some(2) => {
                    let application: ApplicationName =
                        super::decode_text(row.try_get("name")?, "name")?;
                    configuration
                        .applications
                        .get_mut(&application)
                        .expect("connection references a stored application")
                        .connections
                        .insert(
                            row.try_get("alias")?,
                            super::decode_text::<ConnectionReference, _>(
                                row.try_get("value")?,
                                "value",
                            )?,
                        );
                }
                Some(3) => {
                    configuration.settings.integrations.insert(
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

    async fn write(
        &mut self,
        configuration: &EnvironmentConfiguration,
    ) -> Result<bool, sqlx::Error> {
        let mut transaction = self.connection.begin().await?;
        let c = configuration;
        let mode = c.settings.schema.mode.map(|mode| match mode {
            SchemaMode::Reset => "reset",
            SchemaMode::Migrate => "migrate",
        });
        let updated = sqlx::query(
            "UPDATE registry.environment_projects p SET schema_mode=COALESCE($4, \
         (SELECT CASE WHEN e.disposable THEN 'reset' ELSE 'migrate' END \
          FROM registry.environments e WHERE e.org=p.org AND e.env=p.env)) \
         WHERE org=$1 AND env=$2 AND project=$3",
        )
        .bind(c.settings.org.as_str())
        .bind(c.settings.env.as_str())
        .bind(c.settings.project.as_str())
        .bind(mode)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(false);
        }
        // The shared environment owns the readiness budget, drain bound, and target.
        let target: String =
            sqlx::query_scalar("SELECT id::text FROM registry.target WHERE name=$1")
                .bind(&c.settings.target)
                .fetch_one(&mut *transaction)
                .await?;
        sqlx::query(
            "UPDATE registry.environments
         SET readiness_budget_seconds=$3, drain_bound_seconds=$4, target_id=$5::text::uuid
         WHERE org=$1 AND env=$2",
        )
        .bind(c.settings.org.as_str())
        .bind(c.settings.env.as_str())
        .bind(c.readiness_budget_seconds)
        .bind(c.drain_bound_seconds)
        .bind(target)
        .execute(&mut *transaction)
        .await?;
        for (name, integration) in &c.settings.integrations {
            sqlx::query("INSERT INTO registry.environment_integrations (org,env,name,version) VALUES ($1,$2,$3,$4) \
                     ON CONFLICT (org,env,name) DO UPDATE SET version=EXCLUDED.version")
            .bind(c.settings.org.as_str()).bind(c.settings.env.as_str()).bind(name).bind(integration.version).execute(&mut *transaction).await?;
        }
        let applications: Vec<_> = c.applications.keys().map(ApplicationName::as_str).collect();
        sqlx::query("DELETE FROM registry.environment_applications WHERE org=$1 AND env=$2 AND project=$3 AND NOT(application=ANY($4))")
        .bind(c.settings.org.as_str()).bind(c.settings.env.as_str()).bind(c.settings.project.as_str()).bind(&applications).execute(&mut *transaction).await?;
        sqlx::query("DELETE FROM registry.environment_application_connections WHERE org=$1 AND env=$2 AND project=$3")
        .bind(c.settings.org.as_str()).bind(c.settings.env.as_str()).bind(c.settings.project.as_str()).execute(&mut *transaction).await?;
        for (name, application) in &c.applications {
            sqlx::query(
                "INSERT INTO registry.environment_applications \
             (org,env,project,application,label,client_package,client_web) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (org,env,project,application) DO UPDATE SET label=EXCLUDED.label, \
             client_package=EXCLUDED.client_package, client_web=EXCLUDED.client_web",
            )
            .bind(c.settings.org.as_str())
            .bind(c.settings.env.as_str())
            .bind(c.settings.project.as_str())
            .bind(name.as_str())
            .bind(&application.label)
            .bind(
                application
                    .client
                    .as_ref()
                    .map(|client| client.package.as_str()),
            )
            .bind(
                application
                    .client
                    .as_ref()
                    .map(|client| client.web.to_string()),
            )
            .execute(&mut *transaction)
            .await?;
            for (alias, connection) in &application.connections {
                sqlx::query(
                "INSERT INTO registry.environment_application_connections (org,env,project,application,alias,connection_name) \
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(c.settings.org.as_str()).bind(c.settings.env.as_str()).bind(c.settings.project.as_str()).bind(name.as_str())
            .bind(alias).bind(connection.as_str()).execute(&mut *transaction).await?;
            }
        }
        transaction.commit().await?;
        Ok(true)
    }

    async fn retain_integrations(
        &mut self,
        org: &wamn_registry::OrgId,
        env: &wamn_registry::EnvName,
        names: &[&str],
    ) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM registry.environment_integrations WHERE org=$1 AND env=$2 AND NOT(name=ANY($3))")
        .bind(org.as_str()).bind(env.as_str()).bind(names).execute(&mut *self.connection).await?;
        Ok(())
    }
}
