//! Reader authority measured through authenticated sessions.

#[path = "support/grants.rs"]
mod grants;
#[path = "support/system_reader.rs"]
pub mod reader;
#[path = "support/family_surface.rs"]
pub mod surface;
use reader::*;
use wamn_control_provision::{SystemReader, WorkloadRoleFamily};

#[sqlx::test(migrations = false)]
async fn the_connection_reader_cannot_read_or_change_credential_metadata_directly(
    pool: sqlx::PgPool,
) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let admin = test_database.as_str();
    reset_roles(admin, test_database.path().trim_start_matches('/'));
    apply_system_schema(admin);
    run_admin(
        admin,
        "grant the connection reader surface",
        &wamn_control_provision::sql::grant_connection_reader_surface_sql(),
    );
    let role = generation_role(
        SystemReader::Connection,
        test_database.path().trim_start_matches('/'),
    );
    run_admin(
        admin,
        "create an authenticated reader",
        &wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::ConnectionReader,
            test_database.path().trim_start_matches('/'),
            &role,
            READER_PW,
        ),
    );
    let reader = role_url(admin, &role, READER_PW);
    assert_eq!(
        reader_sqlstate(&reader, "SELECT token_hash FROM identity.pats").as_deref(),
        Some("42501")
    );
    run_admin(
        &reader,
        "assert that the reader cannot bypass permissions",
        "DO $$ BEGIN ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname=current_user); END $$;",
    );
    assert_eq!(
        reader_sqlstate(&reader, "SELECT id FROM registry.connection"),
        Some("42501".to_owned()),
        "the connection reader cannot directly read connection rows"
    );
    assert_eq!(
        reader_sqlstate(&reader, "DELETE FROM registry.connection"),
        Some("42501".to_owned()),
        "the connection reader cannot change connection rows"
    );

    for (relation, column) in [
        ("identity.credential", "version"),
        ("registry.external_system", "name"),
        ("registry.integration", "name"),
        ("registry.connection", "status"),
        ("registry.connection_generation", "generation"),
    ] {
        for statement in [
            format!("SELECT * FROM {relation}"),
            format!("UPDATE {relation} SET {column}={column}"),
            format!("DELETE FROM {relation}"),
        ] {
            assert_eq!(
                reader_sqlstate(&reader, &statement),
                Some("42501".to_owned()),
                "the reader cannot access {relation} directly"
            );
        }
    }
}

#[sqlx::test(migrations = false)]
async fn connection_reader_refusals_and_catalog_rules(pool: sqlx::PgPool) {
    grants::family(pool, WorkloadRoleFamily::ConnectionReader).await;
}
