//! SQLx owns the two independent platform migration scopes.

use anyhow::Context as _;
use sqlx::migrate::Migrator;

/// The platform SQL scope selected by the environment lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationDatabase {
    System,
    Project,
}

pub static SYSTEM: Migrator = sqlx::migrate!("../../deploy/sql/migrations/system");
pub static PROJECT: Migrator = sqlx::migrate!("../../deploy/sql/migrations/project");

/// Borrow the selected immutable image migrations.
pub fn embedded(target: MigrationDatabase) -> &'static Migrator {
    match target {
        MigrationDatabase::System => &SYSTEM,
        MigrationDatabase::Project => &PROJECT,
    }
}

impl MigrationDatabase {
    /// Owner of the selected database.
    pub fn owner_role(self) -> String {
        match self {
            Self::System => wamn_role::SYSTEM_ROLE.to_owned(),
            Self::Project => crate::project_migration::login().to_string(),
        }
    }

    /// One bounded migration login for this database and platform scope.
    pub fn login(self, database: &str) -> wamn_role::LoginName {
        match self {
            Self::Project => crate::project_migration::login(),
            Self::System => {
                let digest = wamn_digest::canonical_json_sha256(&serde_json::json!([
                    "platform-migration",
                    wamn_role::SYSTEM_ROLE,
                    database
                ]));
                wamn_registry::name::credential_login(
                    wamn_role::SYSTEM_MIGRATION_ROLE,
                    &digest.hex()[..40],
                )
            }
        }
    }

    /// The Kubernetes Secret that holds the selected migration login's certificate and key.
    pub fn credential_secret(self, database: &str) -> String {
        let login = self.login(database);
        match self {
            Self::Project => {
                wamn_registry::name::project_login_certificate(database, login.as_str())
            }
            Self::System => format!("{login}-tls").replace('_', "-"),
        }
    }

    /// Project the managed credential without changing an explicit operator connection.
    pub fn configuration(self, input: &str) -> anyhow::Result<String> {
        let input = crate::configuration(input)?;
        if !input.trim_start().starts_with('{') {
            return Ok(input.into_owned());
        }
        let mut config: crate::ConnectionConfig = serde_json::from_str(&input)?;
        config.username = self.login(&config.database);
        config.credentials.name = self.credential_secret(&config.database);
        config.to_json()
    }

    /// Refuse administrator sessions and role switching before platform SQL executes.
    pub async fn require_login(
        self,
        connection: &mut sqlx::PgConnection,
    ) -> anyhow::Result<wamn_role::LoginName> {
        if self == Self::Project {
            return crate::project_migration::require_login(connection).await;
        }
        let (session, current, database, restricted, member): (String, String, String, bool, bool) =
            sqlx::query_as(
                "SELECT session_user::text, current_user::text, current_database()::text, \
             NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls), \
             pg_has_role(session_user,$1,'USAGE') FROM pg_roles WHERE rolname=session_user",
            )
            .bind(wamn_role::SYSTEM_ROLE)
            .fetch_one(connection)
            .await?;
        let session =
            wamn_role::LoginName::try_from(session).context("decode the session login")?;
        let current =
            wamn_role::LoginName::try_from(current).context("decode the current login")?;
        anyhow::ensure!(
            session == self.login(&database) && current == session && restricted && member,
            "platform migrations require their direct restricted managed login"
        );
        Ok(session)
    }
}
