//! Real-PostgreSQL test of the org grants
//! (docs/architecture/overview.md#organization-membership-and-administration):
//! Administration is inherited from explicit grants. Invalid targets are refused.

use wamn_control_provision::{PlatformComponent, SYSTEM_SCHEMA_SQL};
use wamn_platform_identity::{
    IdentityErrorType, create_or_reuse_user,
    org::{activate_org_membership, grant_org_admin, grant_project_admin},
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
    INSERT INTO registry.environments (org, env, policy_name) VALUES \
      ('acme', 'dev', 'dev'), ('acme', 'prod', 'prod'), ('other', 'dev', 'dev'); \
    INSERT INTO registry.environment_projects (org, env, project, secret_name, instance_suffix) VALUES \
      ('acme', 'dev', 'billing', 's1', 'aaaaaaa1'), ('acme', 'prod', 'billing', 's2', 'aaaaaaa2'), \
      ('acme', 'dev', 'shop', 's3', 'aaaaaaa3'), ('other', 'dev', 'billing', 's4', 'aaaaaaa4');";

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
    client
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&PlatformComponent::Provisioning.principal_id().to_string()],
        )
        .await
        .expect("bind wamn:provisioning for the fixture session");
    client.batch_execute(SEED_SQL).await.expect("seed the orgs");
    let ann = create_or_reuse_user(&client, "ann@example.test", "Ann")
        .await
        .expect("create ann")
        .principal_id;
    let ben = create_or_reuse_user(&client, "ben@example.test", "Ben")
        .await
        .expect("create ben")
        .principal_id;

    use wamn_platform_identity::org::{
        grant_environment_admin, revoke_environment_admin, revoke_org_admin,
    };
    use wamn_platform_identity::{control::is_project_admin, has_project_env_membership};
    let refused = grant_org_admin(&client, &ann, "acme").await.unwrap_err();
    assert_eq!(refused.error_type(), IdentityErrorType::NotFound);
    activate_org_membership(&client, &ann, "acme")
        .await
        .unwrap();
    grant_org_admin(&client, &ann, "acme").await.unwrap();
    for row in client
        .query(
            "SELECT org,env,project FROM registry.environment_projects WHERE org='acme'",
            &[],
        )
        .await
        .unwrap()
    {
        let org: String = row.get(0);
        let env: String = row.get(1);
        let project: String = row.get(2);
        assert!(
            is_project_admin(&client, &ann, &org, &project, Some(&env))
                .await
                .unwrap()
        );
        assert!(
            has_project_env_membership(&client, &ann, &org, &project, &env)
                .await
                .unwrap()
        );
    }
    assert!(!client.query_one("SELECT EXISTS (SELECT FROM identity.project_roles WHERE principal_id=$1::text::uuid) OR EXISTS (SELECT FROM identity.project_env_memberships WHERE principal_id=$1::text::uuid)",&[&ann.to_string()]).await.unwrap().get::<_,bool>(0));
    activate_org_membership(&client, &ben, "acme")
        .await
        .unwrap();
    grant_environment_admin(&client, &ben, "acme", "dev")
        .await
        .unwrap();
    assert!(
        is_project_admin(&client, &ben, "acme", "shop", Some("dev"))
            .await
            .unwrap()
    );
    assert!(
        !is_project_admin(&client, &ben, "acme", "shop", Some("prod"))
            .await
            .unwrap()
    );
    assert!(
        !wamn_platform_identity::control::is_org_admin(&client, &ben, "acme")
            .await
            .unwrap()
    );
    assert!(
        grant_project_admin(&client, &ben, "acme", "missing")
            .await
            .is_err()
    );
    assert!(
        grant_project_admin(&client, &ben, "other", "billing")
            .await
            .is_err()
    );
    assert!(
        grant_environment_admin(&client, &ben, "other", "dev")
            .await
            .is_err()
    );
    let mut drops = wamn_broker::auth_changes::AuthDrops::default();
    revoke_environment_admin(&client, &ben, "acme", "dev", &mut drops)
        .await
        .unwrap();
    assert!(
        !has_project_env_membership(&client, &ben, "acme", "shop", "dev")
            .await
            .unwrap()
    );
    grant_project_admin(&client, &ben, "acme", "billing")
        .await
        .unwrap();
    assert!(
        is_project_admin(&client, &ben, "acme", "billing", Some("prod"))
            .await
            .unwrap()
    );
    assert!(
        !is_project_admin(&client, &ben, "acme", "shop", Some("dev"))
            .await
            .unwrap()
    );
    client.execute("UPDATE identity.org_memberships SET status='inactive' WHERE principal_id=$1::text::uuid",&[&ben.to_string()]).await.unwrap();
    assert!(
        !has_project_env_membership(&client, &ben, "acme", "billing", "dev")
            .await
            .unwrap()
    );
    assert!(
        grant_project_admin(&client, &ben, "acme", "shop")
            .await
            .is_err()
    );
    grant_project_admin(&client, &ann, "acme", "billing")
        .await
        .unwrap();
    revoke_org_admin(&client, &ann, "acme", &mut drops)
        .await
        .unwrap();
    assert!(
        is_project_admin(&client, &ann, "acme", "billing", Some("dev"))
            .await
            .unwrap()
    );
    assert!(
        !is_project_admin(&client, &ann, "acme", "shop", Some("dev"))
            .await
            .unwrap()
    );
    drop(client);
    connection_task.await.unwrap();
}
