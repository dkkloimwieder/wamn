//! Shared installation of a login fixture for the definer tests.

use postgres_protocol::escape::escape_identifier;
use wamn_identity_protocol::workload_role::WorkloadRoleIdentityType;
use wamn_identity_protocol::workload_role::{
    WorkloadRoleFamily, WorkloadRoleIdentity, workload_login,
};
use wamn_postgres::migration_scope::MigrationDatabase;

#[derive(Debug)]
pub struct Fixture {
    pub admin: sqlx::PgConnection,
    pub url: String,
    pub login: wamn_role::LoginName,
}

pub async fn execute(connection: &mut sqlx::PgConnection, statement: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(connection)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

/// Install the schemas of `family` on the database of `url` and create its login.
pub async fn prepare(url: url::Url, family: WorkloadRoleFamily) -> Fixture {
    let database = url.path().trim_start_matches('/');
    let mut admin = wamn_postgres::connect(url.as_str()).await.unwrap();
    match family.identity_type() {
        WorkloadRoleIdentityType::Control | WorkloadRoleIdentityType::Org => {
            super::database::system(url.as_str()).await;
        }
        _ => {
            // As in production, the migration login owns the project database
            // and installs its schemas.
            execute(
                &mut admin,
                wamn_postgres::project_setup::ensure_project_roles_sql(),
            )
            .await;
            super::database::platform_migration_input(url.as_str(), MigrationDatabase::Project)
                .await;
            let owner = MigrationDatabase::Project.login(database);
            execute(
                &mut admin,
                &format!("SET ROLE {}", escape_identifier(owner.as_str())),
            )
            .await;
            for batch in [
                wamn_postgres::project_setup::CATALOG_SCHEMA_SQL,
                include_str!("../../../../deploy/sql/run-state.sql"),
                include_str!("../../../../deploy/sql/run-queue.sql"),
                wamn_postgres::project_setup::APP_SCHEMA_SQL,
                "RESET ROLE",
            ] {
                execute(&mut admin, batch).await;
            }
        }
    }
    let target = match family.identity_type() {
        WorkloadRoleIdentityType::ProjectEnvKey => WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: &"t1--dev--project".parse().unwrap(),
            database,
        },
        WorkloadRoleIdentityType::ProjectEnvironment => WorkloadRoleIdentity::ProjectEnvironment {
            org: &"acme".parse().unwrap(),
            project: &"billing".parse().unwrap(),
            environment: &"dev".parse().unwrap(),
            database,
        },
        WorkloadRoleIdentityType::Control => WorkloadRoleIdentity::Control {
            org: &"acme".parse().unwrap(),
            project: &"billing".parse().unwrap(),
            environment: &"dev".parse().unwrap(),
            database,
        },
        WorkloadRoleIdentityType::Org => WorkloadRoleIdentity::Org {
            org: &"acme".parse().unwrap(),
            database,
        },
    };
    let login = workload_login(family, target).unwrap();
    execute(
        &mut admin,
        &super::database::login_sql(family, database, &login, "grants-test"),
    )
    .await;
    if family == WorkloadRoleFamily::Migration {
        execute(
            &mut admin,
            &wamn_postgres::project_migration::grants_sql(&login),
        )
        .await;
    }
    let mut url = url;
    url.set_username(login.as_str()).unwrap();
    url.set_password(Some("grants-test")).unwrap();
    Fixture {
        admin,
        url: url.into(),
        login,
    }
}
