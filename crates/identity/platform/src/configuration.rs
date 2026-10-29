//! The platform configuration rows of one project database
//! (docs/architecture/data-access.md#actors).
//!
//! The control host writes `platform.external_system`, `platform.connection`,
//! `platform.integration`,
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

const EXTERNAL_SYSTEM_SQL: &str = "INSERT INTO platform.external_system (id, name) \
    VALUES ($1::text::uuid, $2) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name \
    WHERE external_system.name IS DISTINCT FROM EXCLUDED.name";

const INTEGRATION_SQL: &str = "INSERT INTO platform.integration \
    (id, integration_type, external_system_id, name, status) \
    VALUES ($1::text::uuid, $2, $3::text::uuid, $4, $5) \
    ON CONFLICT (id) DO UPDATE SET integration_type = EXCLUDED.integration_type, \
    external_system_id = EXCLUDED.external_system_id, name = EXCLUDED.name, status = EXCLUDED.status \
    WHERE (integration.integration_type, integration.external_system_id, integration.name, integration.status) \
    IS DISTINCT FROM (EXCLUDED.integration_type, EXCLUDED.external_system_id, EXCLUDED.name, EXCLUDED.status)";

const CONNECTION_SQL: &str = "INSERT INTO platform.connection \
    (id, integration_id, integration_type, status) VALUES ($1::text::uuid, $2::text::uuid, $3, $4) \
    ON CONFLICT (id) DO UPDATE SET integration_id = EXCLUDED.integration_id, \
    integration_type = EXCLUDED.integration_type, status = EXCLUDED.status \
    WHERE (connection.integration_id, connection.integration_type, connection.status) \
    IS DISTINCT FROM (EXCLUDED.integration_id, EXCLUDED.integration_type, EXCLUDED.status)";

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
    pub id: String,
    pub name: String,
}

/// One `platform.integration` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Integration {
    pub id: String,
    pub integration_type: String,
    pub external_system_id: Option<String>,
    pub name: String,
    pub status: String,
}

/// One `platform.connection` row with effective status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub id: String,
    pub integration_id: String,
    pub integration_type: String,
    pub status: String,
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
    pub integrations: Vec<Integration>,
    pub number_sequences: Vec<NumberSequence>,
    pub settings: Vec<Setting>,
}

impl PlatformConfiguration {
    /// Whether the configuration names no row.
    pub fn is_empty(&self) -> bool {
        self.external_systems.is_empty()
            && self.connections.is_empty()
            && self.integrations.is_empty()
            && self.number_sequences.is_empty()
            && self.settings.is_empty()
    }
}

/// Write the rows of `configuration`, and return how many rows were inserted
/// or changed.
///
/// The caller runs this on the `wamn_administration` login in a transaction
/// that binds `app.user_id` and `app.operation` for recorded writes.
/// Projection writes follow foreign key order: external systems, integrations, connections.
pub async fn write_configuration(
    client: &(impl GenericClient + Sync),
    configuration: &PlatformConfiguration,
) -> Result<u64, IdentityError> {
    let mut written = 0;
    for row in &configuration.external_systems {
        written += execute(client, EXTERNAL_SYSTEM_SQL, &[&row.id, &row.name]).await?;
    }
    for row in &configuration.integrations {
        written += execute(
            client,
            INTEGRATION_SQL,
            &[
                &row.id,
                &row.integration_type,
                &row.external_system_id,
                &row.name,
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
                &row.integration_id,
                &row.integration_type,
                &row.status,
            ],
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

/// One sequence default of a package
/// (docs/architecture/data-access.md#counters): its application name from the
/// environment map, its sequence name and its default pattern.
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
