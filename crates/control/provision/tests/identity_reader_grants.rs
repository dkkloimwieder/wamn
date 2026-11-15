//! Reader authority measured through authenticated sessions.

#[path = "support/grants.rs"]
mod grants;
#[path = "support/system_reader.rs"]
pub mod reader;
#[path = "support/family_surface.rs"]
pub mod surface;
use reader::*;
use wamn_control_provision::sql::grant_identity_reader_surface_sql;
use wamn_control_provision::{SystemReader, WorkloadRoleFamily};

#[sqlx::test(migrations = false)]
async fn the_identity_reader_cannot_write_identity_or_reach_registry(pool: sqlx::PgPool) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let admin_url = test_database.as_str().to_owned();
    let database = {
        let (ok, out, err) = psql_tuples(&admin_url, "SELECT current_database()");
        assert!(ok, "read the target database name:\n{err}");
        out.trim().to_owned()
    };

    reset_roles(&admin_url, &database);
    apply_system_schema(&admin_url);

    let mut reader_url = Vec::new();
    for (reader, grant) in [(SystemReader::Identity, grant_identity_reader_surface_sql())] {
        let role = generation_role(reader, &database);
        run_admin(&admin_url, "converge a system reader", &grant);
        // Replayed: the convergent batch must narrow-and-grant to the same state.
        run_admin(&admin_url, "replay a convergent grant", &grant);
        run_admin(
            &admin_url,
            "prepare a system-reader generation",
            &wamn_control_provision::test_database::login_sql(
                reader.family(),
                &database,
                &role,
                READER_PW,
            ),
        );
        reader_url.push((reader, role_url(&admin_url, &role, READER_PW), role));
    }

    // No reader can delegate its privileges to another role.
    for (reader, url, _) in &reader_url {
        let grants: Vec<bool> = sqlx::query_scalar(ROLE_GRANTS_SQL)
            .bind(reader.family().acl_role())
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(grants.into_iter().all(|grantable| !grantable));
        assert_eq!(
            reader_sqlstate(url, "SELECT principal_id FROM platform.login_principal"),
            Some("42501".to_owned())
        );
        assert_eq!(
            reader_sqlstate(url, "SELECT principal_id FROM platform.tx_identity"),
            Some("42501".to_owned())
        );
    }
    let identity_url = &reader_url[0].1;
    let (ok, _, error) = psql_tuples(
        identity_url,
        "SELECT principal_id, org, status FROM identity.org_memberships",
    );
    assert!(
        ok,
        "the identity reader can read active membership: {error}"
    );
    for denied in [
        "SELECT updated_by FROM identity.org_memberships",
        "SELECT status FROM identity.org_memberships FOR UPDATE",
        "UPDATE identity.org_memberships SET status='active'",
    ] {
        assert_eq!(
            reader_sqlstate(identity_url, denied),
            Some("42501".to_owned()),
            "{denied}"
        );
    }

    // --- each reader's OWN session ------------------------------------------
    for (reader, url, _) in &reader_url {
        run_admin(
            url,
            "a reader session's own self-test",
            "DO $self$ BEGIN \
               ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles \
                            WHERE rolname = current_user), \
                 'this session is superuser or bypasses RLS and establishes nothing'; \
             END $self$;",
        );
        // Neither reader may reach the OTHER's plane, in either direction.
        let foreign = match reader {
            SystemReader::Identity => "SELECT publication FROM registry.event_readers",
            SystemReader::Connection => "SELECT token_hash FROM identity.pats",
        };
        assert_eq!(
            reader_sqlstate(url, foreign).as_deref(),
            Some("42501"),
            "the {reader} reached the other reader's plane"
        );
    }

    let identity_url = &reader_url[0].1;
    // The REAL consuming statements, executed by the REAL credential.
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
        assert_eq!(
            reader_sqlstate(identity_url, statement),
            None,
            "the identity reader cannot run a statement it exists for"
        );
    }
    // …and the two forgeries that credential used to permit.
    for (statement, what) in [
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
        assert_eq!(
            reader_sqlstate(identity_url, statement).as_deref(),
            Some("42501"),
            "the identity reader was able to {what}"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn identity_reader_refusals_and_catalog_rules(pool: sqlx::PgPool) {
    grants::family(pool, WorkloadRoleFamily::IdentityReader).await;
}
