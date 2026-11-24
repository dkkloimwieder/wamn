//! Platform security definers keep restricted authority.

pub mod common;
use common::grants;
use wamn_identity_protocol::workload_role::WorkloadRoleFamily;
use wamn_postgres::migration_scope::MigrationDatabase;

#[tokio::test]
#[ignore = "requires: docker"]
async fn platform_security_definers_have_restricted_authority() {
    security_definer_rules(MigrationDatabase::System).await;
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn project_platform_security_definers_have_restricted_authority() {
    security_definer_rules(MigrationDatabase::Project).await;
}

async fn security_definer_rules(scope: MigrationDatabase) {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let url = postgres.create_database("platform_logins").await.unwrap();
    let mut admin = wamn_postgres::connect(url.as_str()).await.unwrap();
    let existing_schemas: Vec<String> =
        sqlx::query_scalar("SELECT nspname::text FROM pg_namespace")
            .fetch_all(&mut admin)
            .await
            .unwrap();
    let _fixture = match scope {
        MigrationDatabase::System => {
            common::database::system(url.as_str()).await;
            None
        }
        MigrationDatabase::Project => {
            Some(grants::prepare(url, WorkloadRoleFamily::Migration).await)
        }
    };

    // The product bootstrap defines the schema set, including schemas added later.
    let violations: Vec<String> = sqlx::query_scalar(
        "SELECT p.oid::regprocedure::text FROM pg_proc p
         JOIN pg_namespace n ON n.oid=p.pronamespace
         WHERE p.prosecdef AND n.nspname <> ALL($1::text[])
           AND (p.proowner <> (SELECT datdba FROM pg_database WHERE datname = current_database())
                OR NOT EXISTS (SELECT FROM pg_options_to_table(p.proconfig)
                               WHERE option_name='search_path' AND option_value='pg_catalog')
                OR EXISTS (SELECT FROM aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) a
                           WHERE (a.grantee=0 AND a.privilege_type='EXECUTE')
                              OR (a.grantee<>p.proowner AND a.is_grantable)))",
    )
    .bind(&existing_schemas)
    .fetch_all(&mut admin)
    .await
    .unwrap();
    assert!(
        violations.is_empty(),
        "{scope:?} platform functions expose definer authority: {violations:?}"
    );
}
