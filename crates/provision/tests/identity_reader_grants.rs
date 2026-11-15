//! The identity reader reads permissions without granting or changing them.

#[path = "support/grants.rs"]
mod grants;
use wamn_control_provision::{WorkloadRoleFamily, sql};

#[sqlx::test(migrations = false)]
async fn identity_reader_refusals_and_catalog_rules(pool: sqlx::PgPool) {
    let (mut fixture, _lock) = grants::family(&pool, WorkloadRoleFamily::IdentityReader).await;
    grants::execute(
        &mut fixture.admin,
        &sql::grant_identity_reader_surface_sql(),
    )
    .await;
    let mut reader = wamn_postgres::connect(&fixture.url).await.unwrap();
    grants::execute(
        &mut reader,
        "SELECT principal_id,org,status FROM identity.org_memberships",
    )
    .await;
    for statement in [
        "SELECT updated_by FROM identity.org_memberships",
        "SELECT * FROM identity.org_memberships",
        "SELECT status FROM identity.org_memberships FOR UPDATE",
        "UPDATE identity.org_memberships SET status='active'",
        "SELECT publication FROM registry.event_readers",
    ] {
        grants::refuses(&fixture.url, statement).await;
    }
    for statement in [
        "SELECT p.id::text, p.type, p.subject, p.display_name, p.status, \
         identity.pats.token_hash, \
         (identity.pats.revoked_at IS NULL AND identity.pats.expires_at > now()) AS usable \
         FROM identity.pats JOIN identity.principals p \
           ON p.id = identity.pats.principal_id \
         WHERE identity.pats.token_prefix = '0123456789abcdef'",
        "SELECT role FROM identity.project_roles \
         WHERE principal_id = '00000000-0000-4000-8000-000000000000'::uuid \
           AND org = 'acme' AND project = 'widgets' ORDER BY role",
        "SELECT EXISTS (SELECT 1 FROM identity.project_env_memberships \
         WHERE principal_id = '00000000-0000-4000-8000-000000000000'::uuid \
           AND org = 'acme' AND project = 'widgets' AND env = 'dev')",
    ] {
        grants::execute(&mut reader, statement).await;
    }
    for (statement, _) in [
        (
            "INSERT INTO identity.pats \
             (principal_id, token_prefix, token_hash, label, expires_at) \
             VALUES ('00000000-0000-4000-8000-000000000000'::uuid, '0123456789abcdef', \
                     repeat('a', 64), 'forged', now() + interval '1 day')",
            "mint a personal access token bound to any principal",
        ),
        (
            "UPDATE identity.pats SET token_hash = repeat('b', 64)",
            "re-point an existing token at a digest it controls",
        ),
        (
            "INSERT INTO identity.project_roles (principal_id, org, project, role) \
             VALUES ('00000000-0000-4000-8000-000000000000'::uuid, 'acme', 'widgets', \
                     'project-admin')",
            "self-grant a project role",
        ),
        (
            "UPDATE identity.project_roles SET role = 'project-admin'",
            "escalate an existing project role",
        ),
        (
            "INSERT INTO identity.project_env_memberships (principal_id, org, project, env) \
             VALUES ('00000000-0000-4000-8000-000000000000'::uuid, 'acme', 'widgets', 'dev')",
            "self-grant environment membership",
        ),
        (
            "UPDATE identity.project_env_memberships SET env = 'prod'",
            "move membership into another environment",
        ),
        (
            "DELETE FROM identity.project_env_memberships",
            "revoke environment memberships",
        ),
        (
            "INSERT INTO identity.principals (type, subject, display_name) \
             VALUES ('service', 'forged', 'forged')",
            "mint a principal",
        ),
    ] {
        grants::refuses(&fixture.url, statement).await;
    }
}
