//! Seeds route-test principals in the installed product control schema.
//!
//! An including test declares the shared session fixture beside this module as `session_fixture`.
use super::session_fixture::{ISSUER, ORG};

/// Seed route-test principals in the installed product control schema.
pub(super) async fn install_authority(
    client: &mut sqlx::PgConnection,
    project: &wamn_registry::ProjectId,
    environment: &wamn_registry::EnvName,
    audience: &str,
    principals: &[&str],
) -> anyhow::Result<()> {
    let actor = wamn_control_provision::PlatformComponent::Provisioning.principal_id();
    let mut transaction =
        wamn_test_postgres::transaction(client, &actor, "admin:seed-session-authority").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::test_database::seed_org_project_env_sql(
            &ORG.parse().unwrap(),
            project,
            environment,
        ),
    ))
    .execute(&mut *transaction)
    .await?;
    for principal in principals {
        sqlx::query("INSERT INTO identity.principals (id,type,subject,email,display_name) VALUES ($1::text::uuid,'user',$1,$1||'@example.test',$1)")
            .bind(principal).execute(&mut *transaction).await?;
        sqlx::query("INSERT INTO identity.org_memberships (principal_id,org,status) VALUES ($1::text::uuid,$2,'active')")
            .bind(principal).bind(ORG).execute(&mut *transaction).await?;
        sqlx::query("INSERT INTO identity.project_env_memberships (principal_id,org,project,env) VALUES ($1::text::uuid,$2,$3,$4)")
            .bind(principal).bind(ORG).bind(project).bind(environment).execute(&mut *transaction).await?;
        sqlx::query("INSERT INTO identity.password_logins (id,principal_id,issuer,audience,authenticated_at,expires_at,renewal_expires_at) VALUES ($1::text::uuid,$1::text::uuid,$2,$3,now(),now()+interval '8 hours',now()+interval '30 minutes')")
            .bind(principal).bind(ISSUER).bind(audience).execute(&mut *transaction).await?;
    }
    transaction.commit().await?;
    Ok(())
}
