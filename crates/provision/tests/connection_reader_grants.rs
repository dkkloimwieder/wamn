//! The connection reader cannot access credential metadata directly.

#[path = "support/grants.rs"]
mod grants;
use wamn_control_provision::WorkloadRoleFamily;

#[sqlx::test(migrations = false)]
async fn connection_reader_refusals_and_catalog_rules(pool: sqlx::PgPool) {
    let (fixture, _lock) = grants::family(&pool, WorkloadRoleFamily::ConnectionReader).await;
    for statement in [
        "SELECT token_hash FROM identity.pats",
        "SELECT id FROM registry.connection",
        "DELETE FROM registry.connection",
    ] {
        grants::refuses(&fixture.url, statement).await;
    }
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
            grants::refuses(&fixture.url, &statement).await;
        }
    }
}
