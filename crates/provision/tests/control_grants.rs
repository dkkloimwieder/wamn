//! Database privileges measured through the login and the PostgreSQL catalog.

#[path = "support/grants.rs"]
mod grants;
use wamn_control_provision::WorkloadRoleFamily;

#[sqlx::test(migrations = false)]
async fn control_routes_cannot_write_release_connections_or_bindings(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let fixture = grants::prepare(&pool, WorkloadRoleFamily::Control).await;
    for statement in [
        "INSERT INTO catalog.release_connection DEFAULT VALUES",
        "DELETE FROM catalog.connection_binding",
    ] {
        grants::refuses(&fixture.url, statement).await;
    }
}

#[sqlx::test(migrations = false)]
async fn control_refusals_and_catalog_rules(pool: sqlx::PgPool) {
    grants::family(pool, WorkloadRoleFamily::Control).await;
}
