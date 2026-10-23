//! Keyset queries against an owned, disposable PostgreSQL server.

use wamn_control_provision::{PlatformComponent, SYSTEM_SCHEMA_SQL};
use wamn_platform_identity::{control, create_or_reuse_user, org};

#[tokio::test]
async fn sql_pages_keep_order_scope_and_current_authority() {
    let server = wamn_test_postgres::start(&[]).expect("start disposable PostgreSQL");
    let database = server.database("postgres").unwrap();
    let (client, connection) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let driver = tokio::spawn(connection);
    client
        .batch_execute("CREATE ROLE wamn_system")
        .await
        .unwrap();
    client.batch_execute(SYSTEM_SCHEMA_SQL).await.unwrap();
    client
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&PlatformComponent::Provisioning.principal_id().to_string()],
        )
        .await
        .unwrap();
    client
        .batch_execute(
            "INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES
           ('acme', 'pooled', 'wamn-pg'), ('other', 'pooled', 'wamn-pg');
         INSERT INTO registry.projects (org, id) VALUES
           ('acme', 'a'), ('acme', 'b'), ('acme', 'c'), ('other', 'a');
         INSERT INTO registry.env_policies
           (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
           VALUES ('acme', 'dev', '\"own\"', 0, 1, '1Gi', '1', '1Gi', 'postgres:18');
         INSERT INTO registry.project_envs
           (org, project, env, secret_name, instance_suffix, policy_name) VALUES
           ('acme', 'a', 'a', 's1', 'aaaaaaa1', 'dev'),
           ('acme', 'a', 'b', 's2', 'aaaaaaa2', 'dev'),
           ('acme', 'a', 'c', 's3', 'aaaaaaa3', 'dev');",
        )
        .await
        .unwrap();
    let mut principals = Vec::new();
    for email in ["c@example.test", "a@example.test", "b@example.test"] {
        let principal = create_or_reuse_user(&client, email, email)
            .await
            .unwrap()
            .principal_id;
        org::activate_org_membership(&client, &principal, "acme")
            .await
            .unwrap();
        org::grant_project_admin(&client, &principal, "acme", "a")
            .await
            .unwrap();
        principals.push(principal);
    }
    let caller = &principals[0];
    org::activate_org_membership(&client, caller, "other")
        .await
        .unwrap();
    org::grant_project_admin(&client, caller, "other", "a")
        .await
        .unwrap();
    for project in ["b", "c"] {
        org::grant_project_admin(&client, caller, "acme", project)
            .await
            .unwrap();
    }

    let projects = control::org_projects(&client, "acme").await.unwrap();
    assert_eq!(
        projects
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert_eq!(
        control::org_projects_page(&client, "acme", None, Some(2))
            .await
            .unwrap(),
        projects[..2]
    );
    assert_eq!(
        control::org_projects_page(&client, "acme", Some("b"), Some(2))
            .await
            .unwrap(),
        projects[2..]
    );
    assert!(
        control::org_projects_page(&client, "acme", Some("c"), Some(2))
            .await
            .unwrap()
            .is_empty()
    );
    let environments = org::project_envs(&client, "acme", "a").await.unwrap();
    assert_eq!(
        environments
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert_eq!(
        org::project_envs_page(&client, "acme", "a", None, Some(2))
            .await
            .unwrap(),
        environments[..2]
    );
    assert_eq!(
        org::project_envs_page(&client, "acme", "a", Some("b"), Some(2))
            .await
            .unwrap(),
        environments[2..]
    );
    assert!(
        org::project_envs_page(&client, "other", "a", None, Some(2))
            .await
            .unwrap()
            .is_empty()
    );

    let users = org::org_users(&client, "acme").await.unwrap();
    assert_eq!(
        users
            .iter()
            .map(|row| row.email.as_str())
            .collect::<Vec<_>>(),
        ["a@example.test", "b@example.test", "c@example.test"]
    );
    assert_eq!(
        org::org_users_page(&client, "acme", None, None, Some(2))
            .await
            .unwrap(),
        users[..2]
    );
    assert_eq!(
        org::org_users_page(
            &client,
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
    client
        .execute(
            "DELETE FROM identity.project_env_memberships \
             WHERE principal_id = $1::text::uuid AND env = 'b'",
            &[&principals[2].as_str()],
        )
        .await
        .unwrap();
    let members = org::project_members(&client, "acme", "a").await.unwrap();
    assert_eq!(members.len(), 3);
    assert_eq!(members[0].environments, ["a", "b", "c"]);
    // The filter applies before the keyset and the limit, and narrows
    // `environments` to the one environment.
    let filtered = org::project_members_page(&client, "acme", "a", Some("b"), None, None, Some(1))
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
        &client,
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
        org::project_members_page(&client, "acme", "a", Some("zzz"), None, None, Some(2))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        org::project_members_page(&client, "acme", "a", Some(""), None, None, Some(2))
            .await
            .is_err()
    );
    assert_eq!(
        org::project_members_page(&client, "acme", "a", None, None, None, Some(2))
            .await
            .unwrap(),
        members[..2]
    );
    assert_eq!(
        org::project_members_page(
            &client,
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
            &client,
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
        control::control_projects(&client, caller, "acme")
            .await
            .unwrap(),
        ["a", "b", "c"]
    );
    assert_eq!(
        control::control_projects_page(&client, caller, "acme", None, Some(2))
            .await
            .unwrap(),
        ["a", "b"]
    );
    assert_eq!(
        control::control_projects_page(&client, caller, "acme", Some("b"), Some(2))
            .await
            .unwrap(),
        ["c"]
    );
    assert_eq!(
        control::control_orgs(&client, caller).await.unwrap(),
        ["acme", "other"]
    );
    assert_eq!(
        control::control_orgs_page(&client, caller, None, None, Some(1))
            .await
            .unwrap(),
        ["acme"]
    );
    assert_eq!(
        control::control_orgs_page(&client, caller, None, Some("acme"), Some(2))
            .await
            .unwrap(),
        ["other"]
    );
    assert_eq!(
        control::control_orgs_page(&client, caller, Some("acme"), None, Some(2))
            .await
            .unwrap(),
        ["acme"]
    );
    org::revoke_project_admin(
        &client,
        caller,
        "acme",
        "c",
        &mut wamn_platform_identity::auth_changes::AuthDrops::default(),
    )
    .await
    .unwrap();
    assert!(
        control::control_projects_page(&client, caller, "acme", Some("b"), Some(2))
            .await
            .unwrap()
            .is_empty()
    );
    drop(client);
    driver.await.unwrap().unwrap();
}
