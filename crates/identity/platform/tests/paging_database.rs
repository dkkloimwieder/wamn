//! Keyset queries against an owned, disposable PostgreSQL server.

use sqlx::{Connection as _, Row as _, Statement as _};
use wamn_control_provision::{PlatformComponent, SYSTEM_SCHEMA_SQL};
use wamn_platform_identity::{control, create_or_reuse_user, org};

#[tokio::test]
async fn sql_pages_keep_order_filter_and_current_authority() {
    let server = wamn_test_postgres::start(&[]).expect("start disposable PostgreSQL");
    let mut database = server.database("postgres").unwrap();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    sqlx::raw_sql("CREATE ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(SYSTEM_SCHEMA_SQL)
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    sqlx::query("SELECT set_config('app.user_id', $1, false)")
        .bind(&PlatformComponent::Provisioning.principal_id().to_string())
        .execute(&mut client)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    sqlx::raw_sql("INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES
           ('acme', 'pooled', 'wamn-pg'), ('other', 'pooled', 'wamn-pg');
         INSERT INTO registry.projects (org, id) VALUES
           ('acme', 'a'), ('acme', 'b'), ('acme', 'c'), ('other', 'a');
         INSERT INTO registry.env_policies
           (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
           VALUES ('acme', 'dev', '\"own\"', 0, 1, '1Gi', '1', '1Gi', 'postgres:18');
         WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ('acme','a','dev'),('acme','b','dev'),('acme','c','dev') ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ('acme','a','a','s1','aaaaaaa1'),('acme','a','b','s2','aaaaaaa2'),('acme','a','c','s3','aaaaaaa3');").execute(&mut client).await.map(drop)
        .unwrap();
    let mut principals = Vec::new();
    for email in ["c@example.test", "a@example.test", "b@example.test"] {
        let principal = create_or_reuse_user(&mut client, email, email)
            .await
            .unwrap()
            .principal_id;
        org::activate_org_membership(&mut client, &principal, "acme")
            .await
            .unwrap();
        org::grant_project_admin(&mut client, &principal, "acme", "a")
            .await
            .unwrap();
        principals.push(principal);
    }
    let caller = &principals[0];
    org::activate_org_membership(&mut client, caller, "other")
        .await
        .unwrap();
    org::grant_project_admin(&mut client, caller, "other", "a")
        .await
        .unwrap();
    for project in ["b", "c"] {
        org::grant_project_admin(&mut client, caller, "acme", project)
            .await
            .unwrap();
    }

    let projects = control::org_projects(&mut client, "acme").await.unwrap();
    assert_eq!(
        projects
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert_eq!(
        control::org_projects_page(&mut client, "acme", None, Some(2))
            .await
            .unwrap(),
        projects[..2]
    );
    assert_eq!(
        control::org_projects_page(&mut client, "acme", Some("b"), Some(2))
            .await
            .unwrap(),
        projects[2..]
    );
    assert!(
        control::org_projects_page(&mut client, "acme", Some("c"), Some(2))
            .await
            .unwrap()
            .is_empty()
    );
    let environments = org::project_envs(&mut client, "acme", "a").await.unwrap();
    assert_eq!(
        environments
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert_eq!(
        org::project_envs_page(&mut client, "acme", "a", None, Some(2))
            .await
            .unwrap(),
        environments[..2]
    );
    assert_eq!(
        org::project_envs_page(&mut client, "acme", "a", Some("b"), Some(2))
            .await
            .unwrap(),
        environments[2..]
    );
    assert!(
        org::project_envs_page(&mut client, "other", "a", None, Some(2))
            .await
            .unwrap()
            .is_empty()
    );

    let users = org::org_users(&mut client, "acme").await.unwrap();
    assert_eq!(
        users
            .iter()
            .map(|row| row.email.as_str())
            .collect::<Vec<_>>(),
        ["a@example.test", "b@example.test", "c@example.test"]
    );
    assert_eq!(
        org::org_users_page(&mut client, "acme", None, None, Some(2))
            .await
            .unwrap(),
        users[..2]
    );
    assert_eq!(
        org::org_users_page(
            &mut client,
            "acme",
            Some(&users[1].email),
            Some(&users[1].principal_id),
            Some(2)
        )
        .await
        .unwrap(),
        users[2..]
    );
    // project-admin made every user a member of every environment; b@ leaves `b`.
    sqlx::query(
        "DELETE FROM identity.project_env_memberships \
             WHERE principal_id = $1::text::uuid AND env = 'b'",
    )
    .bind(&principals[2].to_string().as_str())
    .execute(&mut client)
    .await
    .map(|result| result.rows_affected())
    .unwrap();
    let members = org::project_members(&mut client, "acme", "a")
        .await
        .unwrap();
    assert_eq!(members.len(), 3);
    assert_eq!(members[0].environments, ["a", "b", "c"]);
    // The filter applies before the keyset and the limit, and narrows
    // `environments` to the one environment.
    let filtered =
        org::project_members_page(&mut client, "acme", "a", Some("b"), None, None, Some(1))
            .await
            .unwrap();
    assert_eq!(
        filtered
            .iter()
            .map(|row| (row.email.as_str(), row.environments.clone()))
            .collect::<Vec<_>>(),
        [("a@example.test", vec!["b".to_owned()])]
    );
    let rest = org::project_members_page(
        &mut client,
        "acme",
        "a",
        Some("b"),
        Some(&filtered[0].email),
        Some(&filtered[0].principal_id),
        Some(2),
    )
    .await
    .unwrap();
    assert_eq!(
        rest.iter()
            .map(|row| (row.email.as_str(), row.environments.clone()))
            .collect::<Vec<_>>(),
        [("c@example.test", vec!["b".to_owned()])]
    );
    assert!(
        org::project_members_page(&mut client, "acme", "a", Some("zzz"), None, None, Some(2))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        org::project_members_page(&mut client, "acme", "a", Some(""), None, None, Some(2))
            .await
            .is_err()
    );
    assert_eq!(
        org::project_members_page(&mut client, "acme", "a", None, None, None, Some(2))
            .await
            .unwrap(),
        members[..2]
    );
    assert_eq!(
        org::project_members_page(
            &mut client,
            "acme",
            "a",
            None,
            Some(&members[1].email),
            Some(&members[1].principal_id),
            Some(2)
        )
        .await
        .unwrap(),
        members[2..]
    );
    // An equal email still advances by UUID, without skipping the greater UUID.
    assert_eq!(
        org::project_members_page(
            &mut client,
            "acme",
            "a",
            None,
            Some(&members[1].email),
            Some("00000000-0000-0000-0000-000000000000"),
            Some(2)
        )
        .await
        .unwrap(),
        members[1..]
    );

    assert_eq!(
        control::control_projects(&mut client, caller, "acme")
            .await
            .unwrap(),
        ["a", "b", "c"]
    );
    assert_eq!(
        control::control_projects_page(&mut client, caller, "acme", None, Some(2))
            .await
            .unwrap(),
        ["a", "b"]
    );
    assert_eq!(
        control::control_projects_page(&mut client, caller, "acme", Some("b"), Some(2))
            .await
            .unwrap(),
        ["c"]
    );
    assert_eq!(
        control::control_orgs(&mut client, caller).await.unwrap(),
        ["acme", "other"]
    );
    assert_eq!(
        control::control_orgs_page(&mut client, caller, None, None, Some(1))
            .await
            .unwrap(),
        ["acme"]
    );
    assert_eq!(
        control::control_orgs_page(&mut client, caller, None, Some("acme"), Some(2))
            .await
            .unwrap(),
        ["other"]
    );
    assert_eq!(
        control::control_orgs_page(&mut client, caller, Some("acme"), None, Some(2))
            .await
            .unwrap(),
        ["acme"]
    );
    org::revoke_project_admin(
        &mut client,
        caller,
        "acme",
        "c",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await
    .unwrap();
    assert!(
        control::control_projects_page(&mut client, caller, "acme", Some("b"), Some(2))
            .await
            .unwrap()
            .is_empty()
    );
    drop(client);
}
