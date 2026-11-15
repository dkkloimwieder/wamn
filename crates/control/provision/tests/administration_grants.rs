//! Least privilege through the authenticated login and catalog rules.

#[path = "support/grants.rs"]
mod grants;
use wamn_control_provision::WorkloadRoleFamily;

#[sqlx::test(migrations = false)]
async fn administration_refusals_and_catalog_rules(pool: sqlx::PgPool) {
    grants::family(pool, WorkloadRoleFamily::Administration).await;
}
