//! The platform configuration rows of one project database
//! (docs/plan/platform-config.md, Tables).
//!
//! The control host writes `platform.external_system`, `platform.connection`,
//! `platform.project_connection`, `platform.integration_allow`,
//! `platform.number_sequence` and `platform.setting` on the path that writes
//! the application rows of [`crate::application`]: with the environment's
//! `wamn_administration` login, in one transaction on the project database
//! that binds the caller and the route's sealed operation id. The database is
//! the org boundary, so these tables have no tenant column and no row
//! security. The application reads these rows and writes none of them.
//!
//! A write keeps a row whose values are already the given ones, so writing
//! the same configuration twice changes nothing and logs no history. A write
//! never removes a row: a row that the configuration does not name stays.
//! Credentials and targets are control-plane data, so no row here holds
//! them.

use tokio_postgres::GenericClient;

use crate::{IdentityError, database_error};

const EXTERNAL_SYSTEM_SQL: &str = "INSERT INTO platform.external_system \
    (id, integration, name, parameters, status) \
    VALUES ($1::text::uuid, $2, $3, $4::text::jsonb, $5) \
    ON CONFLICT (id) DO UPDATE SET \
      integration = EXCLUDED.integration, name = EXCLUDED.name, \
      parameters = EXCLUDED.parameters, status = EXCLUDED.status \
    WHERE (external_system.integration, external_system.name, \
           external_system.parameters, external_system.status) \
          IS DISTINCT FROM \
          (EXCLUDED.integration, EXCLUDED.name, EXCLUDED.parameters, EXCLUDED.status)";

const CONNECTION_SQL: &str = "INSERT INTO platform.connection \
    (id, integration, external_system_id, name, transport, exchange, parameters, status) \
    VALUES ($1::text::uuid, $2, $3::text::uuid, $4, $5, $6, $7::text::jsonb, $8) \
    ON CONFLICT (id) DO UPDATE SET \
      integration = EXCLUDED.integration, \
      external_system_id = EXCLUDED.external_system_id, name = EXCLUDED.name, \
      transport = EXCLUDED.transport, exchange = EXCLUDED.exchange, \
      parameters = EXCLUDED.parameters, status = EXCLUDED.status \
    WHERE (connection.integration, connection.external_system_id, connection.name, \
           connection.transport, connection.exchange, connection.parameters, \
           connection.status) \
          IS DISTINCT FROM \
          (EXCLUDED.integration, EXCLUDED.external_system_id, EXCLUDED.name, \
           EXCLUDED.transport, EXCLUDED.exchange, EXCLUDED.parameters, EXCLUDED.status)";

const PROJECT_CONNECTION_SQL: &str = "INSERT INTO platform.project_connection \
    (project_id, connection_id) VALUES ($1, $2::text::uuid) \
    ON CONFLICT DO NOTHING";

const INTEGRATION_ALLOW_SQL: &str = "INSERT INTO platform.integration_allow \
    (scope_type, scope_id, integration) VALUES ($1, $2, $3) \
    ON CONFLICT DO NOTHING";

const NUMBER_SEQUENCE_SQL: &str = "INSERT INTO platform.number_sequence \
    (id, application, name, pattern) VALUES ($1::text::uuid, $2, $3, $4) \
    ON CONFLICT (id) DO UPDATE SET \
      application = EXCLUDED.application, name = EXCLUDED.name, pattern = EXCLUDED.pattern \
    WHERE (number_sequence.application, number_sequence.name, number_sequence.pattern) \
          IS DISTINCT FROM (EXCLUDED.application, EXCLUDED.name, EXCLUDED.pattern)";

/// A package's default pattern, written only when the application has no row
/// of that name: an edited pattern stays.
const DEFAULT_NUMBER_SEQUENCE_SQL: &str = "INSERT INTO platform.number_sequence \
    (application, name, pattern) VALUES ($1, $2, $3) \
    ON CONFLICT (application, name) DO NOTHING";

const SETTING_SQL: &str = "INSERT INTO platform.setting \
    (scope_type, scope_id, key, value, locked) \
    VALUES ($1, $2, $3, $4::text::jsonb, $5) \
    ON CONFLICT (scope_type, scope_id, key) DO UPDATE SET \
      value = EXCLUDED.value, locked = EXCLUDED.locked \
    WHERE (setting.value, setting.locked) IS DISTINCT FROM (EXCLUDED.value, EXCLUDED.locked)";

/// One `platform.external_system` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSystem {
    /// A uuid, which the control plane assigns.
    pub id: String,
    /// The catalog code of the integration.
    pub integration: String,
    pub name: String,
    pub parameters: serde_json::Value,
    /// `active` or `disabled`.
    pub status: String,
}

/// One `platform.connection` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// A uuid, which the control plane assigns.
    pub id: String,
    /// The catalog code of the integration.
    pub integration: String,
    /// The external system the connection reaches, if any.
    pub external_system_id: Option<String>,
    pub name: String,
    pub transport: String,
    /// `request` or `event`.
    pub exchange: String,
    pub parameters: serde_json::Value,
    /// `active` or `disabled`.
    pub status: String,
}

/// One `platform.project_connection` row: a connection the project enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConnection {
    pub project_id: String,
    pub connection_id: String,
}

/// One `platform.integration_allow` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationAllow {
    /// `org`, `project` or `environment`.
    pub scope_type: String,
    pub scope_id: String,
    pub integration: String,
}

/// One `platform.number_sequence` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberSequence {
    /// A uuid, which the control plane assigns.
    pub id: String,
    pub application: String,
    pub name: String,
    pub pattern: String,
}

/// One `platform.setting` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    /// `org`, `project`, `application`, `role` or `user`.
    pub scope_type: String,
    pub scope_id: String,
    pub key: String,
    pub value: serde_json::Value,
    pub locked: bool,
}

/// The platform configuration rows to write into one project database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlatformConfiguration {
    pub external_systems: Vec<ExternalSystem>,
    pub connections: Vec<Connection>,
    pub project_connections: Vec<ProjectConnection>,
    pub integration_allows: Vec<IntegrationAllow>,
    pub number_sequences: Vec<NumberSequence>,
    pub settings: Vec<Setting>,
}

impl PlatformConfiguration {
    /// Whether the configuration names no row.
    pub fn is_empty(&self) -> bool {
        self.external_systems.is_empty()
            && self.connections.is_empty()
            && self.project_connections.is_empty()
            && self.integration_allows.is_empty()
            && self.number_sequences.is_empty()
            && self.settings.is_empty()
    }
}

/// Write the rows of `configuration`, and return how many rows were inserted
/// or changed.
///
/// The caller runs this on the `wamn_administration` login in a transaction
/// that binds `app.user_id` and `app.operation`, because every table stamps
/// and logs its writes. The writes go in foreign key order, so a connection
/// can name an external system and a project connection a connection of the
/// same configuration.
pub async fn write_configuration(
    client: &(impl GenericClient + Sync),
    configuration: &PlatformConfiguration,
) -> Result<u64, IdentityError> {
    let mut written = 0;
    for row in &configuration.external_systems {
        written += execute(
            client,
            EXTERNAL_SYSTEM_SQL,
            &[
                &row.id,
                &row.integration,
                &row.name,
                &row.parameters.to_string(),
                &row.status,
            ],
        )
        .await?;
    }
    for row in &configuration.connections {
        written += execute(
            client,
            CONNECTION_SQL,
            &[
                &row.id,
                &row.integration,
                &row.external_system_id,
                &row.name,
                &row.transport,
                &row.exchange,
                &row.parameters.to_string(),
                &row.status,
            ],
        )
        .await?;
    }
    for row in &configuration.project_connections {
        written += execute(
            client,
            PROJECT_CONNECTION_SQL,
            &[&row.project_id, &row.connection_id],
        )
        .await?;
    }
    for row in &configuration.integration_allows {
        written += execute(
            client,
            INTEGRATION_ALLOW_SQL,
            &[&row.scope_type, &row.scope_id, &row.integration],
        )
        .await?;
    }
    for row in &configuration.number_sequences {
        written += execute(
            client,
            NUMBER_SEQUENCE_SQL,
            &[&row.id, &row.application, &row.name, &row.pattern],
        )
        .await?;
    }
    for row in &configuration.settings {
        written += execute(
            client,
            SETTING_SQL,
            &[
                &row.scope_type,
                &row.scope_id,
                &row.key,
                &row.value.to_string(),
                &row.locked,
            ],
        )
        .await?;
    }
    Ok(written)
}

/// One sequence default of a package (`docs/plan/platform-config.md`,
/// "Declaration in wamn.k"): the application whose operations number from it,
/// the sequence name and the default pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceDefault {
    pub application: String,
    pub name: String,
    pub pattern: String,
}

/// Create the `platform.number_sequence` row of each default that the
/// application does not have yet, and return how many rows were created. A
/// row that exists keeps its pattern.
///
/// The caller runs this as [`write_configuration`] runs: on the
/// `wamn_administration` authority, in a transaction that binds `app.user_id`
/// and `app.operation`.
pub async fn create_default_sequences(
    client: &(impl GenericClient + Sync),
    defaults: &[SequenceDefault],
) -> Result<u64, IdentityError> {
    let mut created = 0;
    for row in defaults {
        created += execute(
            client,
            DEFAULT_NUMBER_SEQUENCE_SQL,
            &[&row.application, &row.name, &row.pattern],
        )
        .await?;
    }
    Ok(created)
}

async fn execute(
    client: &(impl GenericClient + Sync),
    statement: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> Result<u64, IdentityError> {
    client
        .execute(statement, params)
        .await
        .map_err(|error| database_error(&error))
}
