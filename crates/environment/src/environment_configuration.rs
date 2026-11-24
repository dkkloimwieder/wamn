//! Settings and number sequences written into one project database.

use wamn_authentication::identity_error::IdentityError;
use wamn_postgres::identity_error::database_error;

const NUMBER_SEQUENCE_SQL: &str = "INSERT INTO platform.number_sequence \
    (id, application, name, pattern) VALUES ($1::text::uuid, $2, $3, $4) \
    ON CONFLICT (id) DO UPDATE SET \
      application = EXCLUDED.application, name = EXCLUDED.name, pattern = EXCLUDED.pattern \
    WHERE (number_sequence.application, number_sequence.name, number_sequence.pattern) \
          IS DISTINCT FROM (EXCLUDED.application, EXCLUDED.name, EXCLUDED.pattern)";

const SETTING_SQL: &str = "INSERT INTO platform.setting \
    (target_type, target_id, key, value, locked) \
    VALUES ($1, $2, $3, $4::text::jsonb, $5) \
    ON CONFLICT (target_type, target_id, key) DO UPDATE SET \
      value = EXCLUDED.value, locked = EXCLUDED.locked \
    WHERE (setting.value, setting.locked) IS DISTINCT FROM (EXCLUDED.value, EXCLUDED.locked)";

/// One `platform.number_sequence` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberSequence {
    /// A uuid, which the control plane assigns.
    pub id: String,
    pub application: wamn_registry::ApplicationName,
    pub name: String,
    pub pattern: String,
}

/// One `platform.setting` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    /// `org`, `project`, `application`, `role` or `user`.
    pub target_type: String,
    pub target_id: String,
    pub key: String,
    pub value: serde_json::Value,
    pub locked: bool,
}

/// The platform configuration rows to write into one project database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlatformConfiguration {
    pub number_sequences: Vec<NumberSequence>,
    pub settings: Vec<Setting>,
}

impl PlatformConfiguration {
    /// Whether the configuration names no row.
    pub fn is_empty(&self) -> bool {
        self.number_sequences.is_empty() && self.settings.is_empty()
    }
}

/// Write the rows of `configuration`, and return how many rows were inserted
/// or changed.
///
/// The caller runs this on the `wamn_platform` login in a transaction
/// that binds `app.user_id` and `app.operation` for recorded writes.
pub async fn write_configuration(
    client: &mut sqlx::PgConnection,
    configuration: &PlatformConfiguration,
) -> Result<u64, IdentityError> {
    let mut written = 0;
    for row in &configuration.number_sequences {
        written += sqlx::query(NUMBER_SEQUENCE_SQL)
            .bind(&row.id)
            .bind(row.application.as_str())
            .bind(&row.name)
            .bind(&row.pattern)
            .execute(&mut *client)
            .await
            .map(|result| result.rows_affected())
            .map_err(|error| database_error(&error))?;
    }
    for row in &configuration.settings {
        written += sqlx::query(SETTING_SQL)
            .bind(&row.target_type)
            .bind(&row.target_id)
            .bind(&row.key)
            .bind(row.value.to_string())
            .bind(row.locked)
            .execute(&mut *client)
            .await
            .map(|result| result.rows_affected())
            .map_err(|error| database_error(&error))?;
    }
    Ok(written)
}
