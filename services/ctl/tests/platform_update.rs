//! Signing-state admission uses the configured restricted database connection.
use sqlx::Connection as _;
use wamn_control_provision::migrations::MigrationDatabase;
use wamn_platform_identity::session_keys::{activate_session_key, publish_session_key};

#[tokio::test]
async fn identity_update_requires_active_signing_state_and_bounds_database_reads()
-> anyhow::Result<()> {
    let database = wamn_control_provision::test_database::system();
    let input = wamn_control_provision::test_database::platform_migration_input(
        database.url(),
        MigrationDatabase::System,
    )
    .await?;
    let mut owner = wamn_postgres::connect(database.url()).await?;
    let limit = std::time::Duration::from_millis(100);
    assert!(
        wamn_platform_install::platform::identity::require_keys(&input, "acme", limit)
            .await
            .is_err()
    );
    let issuer = "https://identity.acme.svc.cluster.local";
    let key = publish_session_key(&mut owner, issuer).await?;
    activate_session_key(&mut owner, issuer, &key.kid).await?;
    wamn_platform_install::platform::identity::require_keys(&input, "acme", limit).await?;
    let mut held = owner.begin().await?;
    sqlx::raw_sql("LOCK TABLE identity.session_keys IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *held)
        .await?;
    assert!(
        wamn_platform_install::platform::identity::require_keys(&input, "acme", limit)
            .await
            .is_err()
    );
    held.rollback().await?;
    wamn_platform_install::platform::identity::require_keys(&input, "acme", limit).await?;
    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT FROM identity.session_signing_state WHERE issuer=$1 AND active_kid=$2::text::uuid)")
        .bind(issuer).bind(&key.kid).fetch_one(&mut owner).await?;
    assert!(active);
    Ok(())
}
