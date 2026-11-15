//! Fixture authority for real Receiving commands.

use sqlx::PgConnection as Client;

use anyhow::{Context as _, Result};

/// The provisioned test principal that Receiving fixtures write as.
pub const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";

/// Create the fixture principal under its transaction identity.
///
/// The record-history triggers stamp and log each fixture write with this
/// principal and operation. The first binding in a database creates its user
/// row, and that row stamps itself.
pub async fn bind_fixture_principal(client: &mut Client) -> Result<()> {
    sqlx::raw_sql("BEGIN").execute(&mut *client).await?;
    sqlx::query(
        "INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1::text::uuid,'','fixture','admin:seed-receiving-test-fixture')",
    )
    .bind(&FIXTURE_PRINCIPAL)
    .execute(&mut *client)
    .await
    .map(|result| result.rows_affected())
    .context("bind the fixture test principal")?;
    sqlx::query(
        "INSERT INTO platform.\"user\" (id, type, email) \
             VALUES ($1::text::uuid, 'user', 'fixture@example.invalid') \
             ON CONFLICT DO NOTHING",
    )
    .bind(&FIXTURE_PRINCIPAL)
    .execute(&mut *client)
    .await
    .map(|result| result.rows_affected())
    .context("create the fixture test principal")?;
    sqlx::raw_sql("COMMIT").execute(&mut *client).await?;
    Ok(())
}
