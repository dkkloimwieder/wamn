//! Real-PostgreSQL test of the org grants (docs/plan/platform-ui.md §4.4):
//! `org-admin` and `project-admin` write their row, then the project roles and
//! memberships below it, and refuse a scope outside the member's org.

use tokio_postgres::Client;
use wamn_control_provision::{PlatformComponent, SYSTEM_SCHEMA_SQL};
use wamn_platform_identity::{
    IdentityErrorType, PrincipalId, create_or_reuse_user,
    org::{
        activate_org_membership, grant_org_admin, grant_project_admin, materialize_admin_grants,
    },
};

/// Two orgs. `acme` has two projects and three environments. `other` has a
/// project with the same name.
const SEED_SQL: &str = "\
    INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES \
      ('acme', 'pooled', 'wamn-pg'), ('other', 'pooled', 'wamn-pg'); \
    INSERT INTO registry.env_policies \
      (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) VALUES \
      ('acme', 'dev', '\"own\"', 0, 1, '1Gi', '1', '1Gi', 'postgres:18'), \
      ('acme', 'prod', '\"own\"', 1, 1, '1Gi', '1', '1Gi', 'postgres:18'), \
      ('other', 'dev', '\"own\"', 0, 1, '1Gi', '1', '1Gi', 'postgres:18'); \
    INSERT INTO registry.projects (org, id) VALUES \
      ('acme', 'billing'), ('acme', 'shop'), ('other', 'billing'); \
    INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix) VALUES \
      ('acme', 'billing', 'dev', 's1', 'aaaaaaa1'), ('acme', 'billing', 'prod', 's2', 'aaaaaaa2'), \
      ('acme', 'shop', 'dev', 's3', 'aaaaaaa3'), ('other', 'billing', 'dev', 's4', 'aaaaaaa4');";

/// The org roles, project roles and memberships of one principal, one row per
/// line.
async fn grants(client: &Client, principal: &PrincipalId) -> Vec<String> {
    client
        .query(
            "SELECT 'org ' || org || ' ' || role FROM identity.org_roles \
               WHERE principal_id = $1::text::uuid \
             UNION ALL SELECT 'project ' || org || '/' || project || ' ' || role \
               FROM identity.project_roles WHERE principal_id = $1::text::uuid \
             UNION ALL SELECT 'env ' || org || '/' || project || '/' || env \
               FROM identity.project_env_memberships WHERE principal_id = $1::text::uuid \
             ORDER BY 1",
            &[&principal.as_str()],
        )
        .await
        .expect("read the grants")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn org_grants_write_their_rows_and_refuse_outside_the_org() {
    // The system schema creates the cluster-wide wamn_system and wamn_db_owner roles.
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let (client, connection) = tokio_postgres::connect(test_database.url(), tokio_postgres::NoTls)
        .await
        .expect("connect the org grant test database");
    let connection_task = tokio::spawn(async move {
        connection.await.expect("drive the org grant test database");
    });
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS identity CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') \
             THEN CREATE ROLE wamn_system; END IF; END $$;",
        )
        .await
        .expect("prepare empty platform schemas");
    client
        .batch_execute(SYSTEM_SCHEMA_SQL)
        .await
        .expect("apply the system schema composition");
    client.batch_execute(SEED_SQL).await.expect("seed the orgs");
    client
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&PlatformComponent::Provisioning.principal_id().to_string()],
        )
        .await
        .expect("bind wamn:provisioning for the fixture session");
    let ann = create_or_reuse_user(&client, "ann@example.test", "Ann")
        .await
        .expect("create ann")
        .principal_id;
    let ben = create_or_reuse_user(&client, "ben@example.test", "Ben")
        .await
        .expect("create ben")
        .principal_id;

    let refused = grant_org_admin(&client, &ann, "acme")
        .await
        .expect_err("org-admin needs an active membership");
    assert_eq!(refused.error_type(), IdentityErrorType::NotFound);
    assert_eq!(
        refused.to_string(),
        format!("principal {ann} is not an active member of org acme")
    );

    activate_org_membership(&client, &ann, "acme")
        .await
        .expect("ann joins acme");
    for _ in 0..2 {
        grant_org_admin(&client, &ann, "acme")
            .await
            .expect("grant org-admin, then grant it again");
    }
    assert_eq!(
        grants(&client, &ann).await,
        [
            "env acme/billing/dev",
            "env acme/billing/prod",
            "env acme/shop/dev",
            "org acme org-admin",
            "project acme/billing project-admin",
            "project acme/shop project-admin",
        ]
    );

    activate_org_membership(&client, &ben, "acme")
        .await
        .expect("ben joins acme");
    grant_project_admin(&client, &ben, "acme", "billing")
        .await
        .expect("grant project-admin in billing");
    assert_eq!(
        grants(&client, &ben).await,
        [
            "env acme/billing/dev",
            "env acme/billing/prod",
            "project acme/billing project-admin",
        ]
    );
    let outside = grant_project_admin(&client, &ben, "acme", "store")
        .await
        .expect_err("a project outside the org is refused");
    assert_eq!(
        outside.to_string(),
        "project store is not a project of org acme"
    );
    let other = grant_project_admin(&client, &ben, "other", "billing")
        .await
        .expect_err("an org without the membership is refused");
    assert_eq!(
        other.to_string(),
        format!("principal {ben} is not an active member of org other")
    );

    // A new project and a new environment receive the rows of the current
    // administrators, and a second run changes nothing.
    client
        .batch_execute(
            "INSERT INTO registry.env_policies \
               (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) \
               VALUES ('acme', 'test', '\"own\"', 2, 1, '1Gi', '1', '1Gi', 'postgres:18'); \
             INSERT INTO registry.projects (org, id) VALUES ('acme', 'store'); \
             INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix) VALUES \
               ('acme', 'store', 'dev', 's5', 'aaaaaaa5'), ('acme', 'billing', 'test', 's6', 'aaaaaaa6');",
        )
        .await
        .expect("add a project and an environment");
    for _ in 0..2 {
        materialize_admin_grants(&client, "acme", "store", "dev")
            .await
            .expect("materialize the new project");
        materialize_admin_grants(&client, "acme", "billing", "test")
            .await
            .expect("materialize the new environment");
    }
    assert_eq!(
        grants(&client, &ann).await,
        [
            "env acme/billing/dev",
            "env acme/billing/prod",
            "env acme/billing/test",
            "env acme/shop/dev",
            "env acme/store/dev",
            "org acme org-admin",
            "project acme/billing project-admin",
            "project acme/shop project-admin",
            "project acme/store project-admin",
        ]
    );
    assert_eq!(
        grants(&client, &ben).await,
        [
            "env acme/billing/dev",
            "env acme/billing/prod",
            "env acme/billing/test",
            "project acme/billing project-admin",
        ]
    );

    // An inactive membership refuses a grant, and activation makes it active.
    client
        .execute(
            "UPDATE identity.org_memberships SET status = 'inactive' \
             WHERE principal_id = $1::text::uuid",
            &[&ben.as_str()],
        )
        .await
        .expect("deactivate ben");
    assert!(
        grant_project_admin(&client, &ben, "acme", "shop")
            .await
            .is_err()
    );
    activate_org_membership(&client, &ben, "acme")
        .await
        .expect("ben is active again");
    assert_eq!(
        client
            .query_one(
                "SELECT status FROM identity.org_memberships WHERE principal_id = $1::text::uuid",
                &[&ben.as_str()],
            )
            .await
            .expect("read ben's membership")
            .get::<_, String>(0),
        "active"
    );

    drop(client);
    connection_task.await.expect("close the test database");
}
