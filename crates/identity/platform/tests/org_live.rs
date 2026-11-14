//! Real-PostgreSQL test of the org grants
//! (docs/architecture/overview.md#organization-membership-and-administration):
//! Administration is inherited from explicit grants. Invalid targets are refused.

use sqlx::{Connection as _, Row as _, Statement as _};
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

#[sqlx::test(migrations = false)]
async fn org_grants_write_their_rows_and_refuse_outside_the_org(pool: sqlx::PgPool) {
    // The system schema creates the cluster-wide wamn_system and wamn_db_owner roles.
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let mut client = wamn_postgres::connect(test_database.as_str())
        .await
        .expect("connect the org grant test database");

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DROP SCHEMA IF EXISTS identity CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') \
             THEN CREATE ROLE wamn_system; END IF; END $$;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("prepare empty platform schemas");
    sqlx::raw_sql(SYSTEM_SCHEMA_SQL)
        .execute(&mut client)
        .await
        .map(drop)
        .expect("apply the system schema composition");
    sqlx::query("SELECT set_config('app.user_id', $1, false)")
        .bind(&PlatformComponent::Provisioning.principal_id().to_string())
        .execute(&mut client)
        .await
        .map(|result| result.rows_affected())
        .expect("bind wamn:provisioning for the fixture session");
    sqlx::raw_sql(SEED_SQL)
        .execute(&mut client)
        .await
        .map(drop)
        .expect("seed the orgs");
    let ann = create_or_reuse_user(&mut client, "ann@example.test", "Ann")
        .await
        .expect("create ann")
        .principal_id;
    let ben = create_or_reuse_user(&mut client, "ben@example.test", "Ben")
        .await
        .expect("create ben")
        .principal_id;

    use wamn_platform_identity::org::{
        grant_environment_admin, revoke_environment_admin, revoke_org_admin,
    };
    use wamn_platform_identity::{control::is_project_admin, has_project_env_membership};
    let refused = grant_org_admin(&mut client, &ann, "acme")
        .await
        .unwrap_err();
    assert_eq!(refused.error_type(), IdentityErrorType::NotFound);
    activate_org_membership(&mut client, &ann, "acme")
        .await
        .unwrap();
    grant_org_admin(&mut client, &ann, "acme").await.unwrap();
    for row in
        sqlx::query("SELECT org,env,project FROM registry.environment_projects WHERE org='acme'")
            .fetch_all(&mut client)
            .await
            .unwrap()
    {
        let org: String = row.get(0);
        let env: String = row.get(1);
        let project: String = row.get(2);
        assert!(
            is_project_admin(&mut client, &ann, &org, &project, Some(&env))
                .await
                .unwrap()
        );
        assert!(
            has_project_env_membership(&mut client, &ann, &org, &project, &env)
                .await
                .unwrap()
        );
    }
    assert!(!sqlx::query("SELECT EXISTS (SELECT FROM identity.project_roles WHERE principal_id=$1::text::uuid) OR EXISTS (SELECT FROM identity.project_env_memberships WHERE principal_id=$1::text::uuid)").bind(&ann.to_string()).fetch_one(&mut client).await.unwrap().get::<bool, _>(0));
    activate_org_membership(&mut client, &ben, "acme")
        .await
        .unwrap();
    grant_environment_admin(&mut client, &ben, "acme", "dev")
        .await
        .unwrap();
    assert!(
        is_project_admin(&mut client, &ben, "acme", "shop", Some("dev"))
            .await
            .unwrap()
    );
    assert!(
        !is_project_admin(&mut client, &ben, "acme", "shop", Some("prod"))
            .await
            .unwrap()
    );
    assert!(
        !wamn_platform_identity::control::is_org_admin(&mut client, &ben, "acme")
            .await
            .unwrap()
    );
    assert!(
        grant_project_admin(&mut client, &ben, "acme", "missing")
            .await
            .is_err()
    );
    assert!(
        grant_project_admin(&mut client, &ben, "other", "billing")
            .await
            .is_err()
    );
    assert!(
        grant_environment_admin(&mut client, &ben, "other", "dev")
            .await
            .is_err()
    );
    let mut drops = wamn_broker::auth_changes::AuthDrops::default();
    revoke_environment_admin(&mut client, &ben, "acme", "dev", &mut drops)
        .await
        .unwrap();
    assert!(
        !has_project_env_membership(&mut client, &ben, "acme", "shop", "dev")
            .await
            .unwrap()
    );
    grant_project_admin(&mut client, &ben, "acme", "billing")
        .await
        .unwrap();
    assert!(
        is_project_admin(&mut client, &ben, "acme", "billing", Some("prod"))
            .await
            .unwrap()
    );
    assert!(
        !is_project_admin(&mut client, &ben, "acme", "shop", Some("dev"))
            .await
            .unwrap()
    );
    sqlx::query(
        "UPDATE identity.org_memberships SET status='inactive' WHERE principal_id=$1::text::uuid",
    )
    .bind(&ben.to_string())
    .execute(&mut client)
    .await
    .map(|result| result.rows_affected())
    .unwrap();
    assert!(
        !has_project_env_membership(&mut client, &ben, "acme", "billing", "dev")
            .await
            .unwrap()
    );
    assert!(
        grant_project_admin(&mut client, &ben, "acme", "shop")
            .await
            .is_err()
    );
    grant_project_admin(&mut client, &ann, "acme", "billing")
        .await
        .unwrap();
    revoke_org_admin(&mut client, &ann, "acme", &mut drops)
        .await
        .unwrap();
    assert!(
        is_project_admin(&mut client, &ann, "acme", "billing", Some("dev"))
            .await
            .unwrap()
    );
    assert!(
        !is_project_admin(&mut client, &ann, "acme", "shop", Some("dev"))
            .await
            .unwrap()
    );
    drop(client);
}
