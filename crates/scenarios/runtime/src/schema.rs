//! Ephemeral schema lifecycle for isolated scenario execution.
//!
//! [`EphemeralSchemaProvisioner`] owns a persistent superuser (admin) connection
//! and creates a fresh schema per scenario from a caller-supplied template DDL,
//! then drops it. The runner's `wamn_app` role is NOSUPERUSER/NOCREATEDB and
//! cannot create schemas — exactly as in production — so all provisioning goes
//! through this admin path, never the guest's pool. Host-side flow seeding runs
//! on the same superuser session ([`admin`](EphemeralSchemaProvisioner::admin),
//! RLS-bypassing; the caller re-scopes `search_path` per case).
//!
//! Per-root-run isolation has a second half: [`case_pool`] builds a fresh
//! `wamn_app` pool bound to the scenario schema. A new plugin per root run means
//! the pool's cached prepared-statement plans never alias a prior run's schema
//! (a plan pins its `search_path`), so N sequential
//! `create schema_run_i → run → drop` cases stay isolated.

use std::fmt;
use std::sync::Arc;

use anyhow::Context as _;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

use wamn_pg_core::{Identifier, InvalidIdentifier};
use wamn_runtime::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};

/// A validated schema name used for isolated scenario execution.
///
/// Scenario schemas deliberately use PostgreSQL's unquoted, lowercase identifier
/// subset. The scenario-specific syntax is enforced here, while PostgreSQL's
/// identifier invariants and representation are owned by [`Identifier`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScenarioSchemaName(Identifier);

/// Why a value cannot name an isolated scenario schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidScenarioSchemaName {
    reason: &'static str,
}

impl InvalidScenarioSchemaName {
    /// The violated scenario-schema invariant.
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for InvalidScenarioSchemaName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for InvalidScenarioSchemaName {}

impl From<InvalidIdentifier> for InvalidScenarioSchemaName {
    fn from(error: InvalidIdentifier) -> Self {
        Self {
            reason: error.reason(),
        }
    }
}

impl ScenarioSchemaName {
    /// Validate the complete schema name before any scenario database effect.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidScenarioSchemaName> {
        let identifier = Identifier::new(value)?;
        let (first, rest) = identifier
            .as_str()
            .as_bytes()
            .split_first()
            .expect("Identifier rejects empty names");
        if !matches!(first, b'a'..=b'z' | b'_') {
            return Err(InvalidScenarioSchemaName {
                reason: "scenario schema name must start with a lowercase ASCII letter or underscore",
            });
        }
        if !rest
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
        {
            return Err(InvalidScenarioSchemaName {
                reason: "scenario schema name must contain only lowercase ASCII letters, digits, or underscores",
            });
        }

        Ok(Self(identifier))
    }

    /// The validated, unquoted PostgreSQL identifier.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    fn quoted(&self) -> String {
        self.0.quoted()
    }
}

impl fmt::Display for ScenarioSchemaName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Owns the superuser connection lifecycle for a scenario sandbox's ephemeral
/// schemas.
pub struct EphemeralSchemaProvisioner {
    admin: Client,
    _conn: JoinHandle<()>,
    template: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

impl std::fmt::Debug for EphemeralSchemaProvisioner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EphemeralSchemaProvisioner")
            .finish_non_exhaustive()
    }
}

impl EphemeralSchemaProvisioner {
    /// Connect the persistent superuser session. `template` renders a case's
    /// table DDL given the schema name (e.g. the flow tables + RLS).
    pub async fn connect(
        admin_url: &str,
        template: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let (admin, conn) = tokio_postgres::connect(admin_url, NoTls)
            .await
            .context("admin connect for the ephemeral schema provisioner")?;
        let handle = tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(Self {
            admin,
            _conn: handle,
            template: Arc::new(template),
        })
    }

    /// The persistent superuser client. Host-side flow seeding runs here
    /// (RLS-bypassing); the caller re-scopes its `search_path` per case.
    pub fn admin(&self) -> &Client {
        &self.admin
    }

    /// Drop-and-recreate `schema` from the template DDL — a fresh case. The
    /// schema starts empty (`AUTHORIZATION postgres`), `wamn_app` gets `USAGE`,
    /// and the template lays down the tables + RLS.
    pub async fn provision_case(&self, schema: &ScenarioSchemaName) -> anyhow::Result<()> {
        self.admin
            .batch_execute(&reset_schema_sql(schema))
            .await
            .context("create ephemeral schema")?;
        self.admin
            .batch_execute(&(self.template)(schema.as_str()))
            .await
            .context("apply template DDL")?;
        Ok(())
    }

    /// Drop `schema` (case teardown).
    pub async fn drop_case(&self, schema: &ScenarioSchemaName) -> anyhow::Result<()> {
        self.admin
            .batch_execute(&drop_schema_sql(schema))
            .await
            .context("drop ephemeral schema")?;
        Ok(())
    }
}

/// Build a fresh `wamn_app` pool bound to `schema` for one root scenario run,
/// keyed to `component_id` with the `tenant` claim. A new plugin per run keeps
/// the pool's cached prepared statements from aliasing a prior run's schema.
/// `cfg` carries the app-role connection URL + pool sizing (its `database_url`
/// must be the NOSUPERUSER `wamn_app` URL).
pub fn case_pool(
    cfg: &WamnPostgresConfig,
    tenant: &str,
    schema: &ScenarioSchemaName,
    component_id: &str,
) -> anyhow::Result<Arc<WamnPostgres>> {
    let pg = Arc::new(WamnPostgres::new(cfg.clone())?);
    pg.set_tenant(component_id, tenant)?;
    pg.set_schema(component_id, schema.as_str())?;
    Ok(pg)
}

fn reset_schema_sql(schema: &ScenarioSchemaName) -> String {
    let schema = schema.quoted();
    format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE; \
         CREATE SCHEMA {schema} AUTHORIZATION postgres; \
         GRANT USAGE ON SCHEMA {schema} TO wamn_app;"
    )
}

fn drop_schema_sql(schema: &ScenarioSchemaName) -> String {
    let schema = schema.quoted();
    format!("DROP SCHEMA IF EXISTS {schema} CASCADE;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_postgresql_63_byte_boundary() {
        let value = format!("s{}", "a".repeat(62));
        let schema = ScenarioSchemaName::new(value.clone()).unwrap();
        assert_eq!(schema.as_str(), value);
        assert_eq!(schema.to_string(), value);
    }

    #[test]
    fn delegates_empty_and_nul_validation_to_postgresql_identifiers() {
        for (value, reason) in [
            ("", "identifier is empty"),
            ("scenario\0case", "identifier contains NUL"),
        ] {
            let error = ScenarioSchemaName::new(value).unwrap_err();
            assert_eq!(error.reason(), reason);
        }
    }

    #[test]
    fn rejects_names_over_63_bytes_without_aliasing() {
        let first = format!("s{}", "a".repeat(63));
        let second = format!("s{}", "a".repeat(62) + "b");
        assert_ne!(first, second);
        assert_eq!(&first[..63], &second[..63]);
        assert!(ScenarioSchemaName::new(first).is_err());
        assert!(ScenarioSchemaName::new(second).is_err());
    }

    #[test]
    fn measures_multibyte_input_in_utf8_bytes() {
        let over_limit = format!("s{}", "é".repeat(32));
        let error = ScenarioSchemaName::new(over_limit).unwrap_err();
        assert_eq!(
            error.reason(),
            "identifier exceeds PostgreSQL's 63-byte limit"
        );

        let within_limit_but_not_bare = format!("s{}", "é".repeat(31));
        assert_eq!(within_limit_but_not_bare.len(), 63);
        let error = ScenarioSchemaName::new(within_limit_but_not_bare).unwrap_err();
        assert_eq!(
            error.reason(),
            "scenario schema name must contain only lowercase ASCII letters, digits, or underscores"
        );
    }

    #[test]
    fn rejects_invalid_bare_identifier_syntax() {
        for value in ["Upper", "1st", "scenario-case", "scenario;drop"] {
            assert!(
                ScenarioSchemaName::new(value).is_err(),
                "{value:?} must be rejected"
            );
        }
    }

    #[test]
    fn scenario_ddl_builders_have_no_raw_string_route() {
        let schema = ScenarioSchemaName::new("scenario_case_1").unwrap();
        assert_eq!(
            reset_schema_sql(&schema),
            "DROP SCHEMA IF EXISTS \"scenario_case_1\" CASCADE; \
             CREATE SCHEMA \"scenario_case_1\" AUTHORIZATION postgres; \
             GRANT USAGE ON SCHEMA \"scenario_case_1\" TO wamn_app;"
        );
        assert_eq!(
            drop_schema_sql(&schema),
            "DROP SCHEMA IF EXISTS \"scenario_case_1\" CASCADE;"
        );
    }

    #[test]
    fn root_runs_get_distinct_plugin_owned_pools() {
        let config = WamnPostgresConfig {
            database_url: None,
            pool_max_size: 1,
            wait_timeout_ms: 1,
            statement_timeout_ms: 1,
            row_limit: 1,
        };
        let first_schema = ScenarioSchemaName::new("scenario_run_0").unwrap();
        let second_schema = ScenarioSchemaName::new("scenario_run_1").unwrap();

        let first = case_pool(&config, "tenant", &first_schema, "flow").unwrap();
        let second = case_pool(&config, "tenant", &second_schema, "flow").unwrap();

        assert!(
            !Arc::ptr_eq(&first, &second),
            "each root run must own a fresh plugin and its prepared-statement pool"
        );
    }
}
