//! Fixture authority for real Receiving commands.

use sqlx::PgConnection as Client;

use anyhow::{Context as _, Result};

/// The provisioned test principal that Receiving fixtures write as.
pub const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";

/// Bind the fixture test principal as `app.user_id` and the fixture purpose as
/// `app.operation` for this session.
///
/// The record-history triggers stamp and log each fixture write with this
/// principal and operation. The first binding in a database creates its user
/// row, and that row stamps itself.
pub async fn bind_fixture_principal(client: &mut Client) -> Result<()> {
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:seed-receiving-test-fixture', false)",
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
    Ok(())
}
