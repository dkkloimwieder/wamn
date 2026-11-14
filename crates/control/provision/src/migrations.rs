//! SQLx owns the two independent platform migration scopes.

use sqlx::migrate::Migrator;

/// The platform SQL scope selected by the environment lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationDatabase {
    System,
    Project,
}

pub static SYSTEM: Migrator = sqlx::migrate!("../../../deploy/sql/migrations/system");
pub static PROJECT: Migrator = sqlx::migrate!("../../../deploy/sql/migrations/project");

/// Borrow the selected immutable image migrations.
pub fn embedded(target: MigrationDatabase) -> &'static Migrator {
    match target {
        MigrationDatabase::System => &SYSTEM,
        MigrationDatabase::Project => &PROJECT,
    }
}

impl MigrationDatabase {
    /// The existing owner role whose privileges the managed login inherits.
    pub const fn owner_role(self) -> &'static str {
        match self {
            Self::System => "wamn_system",
            Self::Project => "wamn_db_owner",
        }
    }

    /// One bounded migration login for this database and platform scope.
    pub fn login(self, database: &str) -> String {
        let digest = wamn_digest::canonical_json_sha256(&serde_json::json!([
            "platform-migration",
            self.owner_role(),
            database
        ]));
        wamn_registry::name::credential_login("wamn_platform_sql", &digest[7..47])
    }

    /// One native basic-auth Secret for the managed login.
    pub fn managed_role(self, database: &str) -> crate::connection::ManagedRole {
        let login = self.login(database);
        let secret = login.replace('_', "-");
        crate::connection::managed_role(login, self.owner_role(), secret)
    }

    /// Project the managed credential without changing an explicit operator connection.
    pub fn configuration(self, input: &str) -> anyhow::Result<String> {
        let input = wamn_postgres::configuration(input)?;
        if !input.trim_start().starts_with('{') {
            return Ok(input.into_owned());
        }
        let mut config: wamn_postgres::ConnectionConfig = serde_json::from_str(&input)?;
        let role = self.managed_role(&config.database);
        config.username = role.name;
        config.credentials.name = role.password_secret.name;
        config.to_json()
    }

    /// Refuse administrator sessions and role switching before platform SQL executes.
    pub async fn require_login(
        self,
        connection: &mut sqlx::PgConnection,
    ) -> anyhow::Result<String> {
        let (session, current, database, restricted, member): (String, String, String, bool, bool) =
            sqlx::query_as(
                "SELECT session_user::text, current_user::text, current_database()::text, \
             NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls), \
             pg_has_role(session_user,$1,'USAGE') FROM pg_roles WHERE rolname=session_user",
            )
            .bind(self.owner_role())
            .fetch_one(connection)
            .await?;
        anyhow::ensure!(
            session == self.login(&database) && current == session && restricted && member,
            "platform migrations require their direct restricted managed login"
        );
        Ok(session)
    }
}
