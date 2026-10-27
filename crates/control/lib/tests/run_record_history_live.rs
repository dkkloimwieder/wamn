//! The policy writer and run-plane reconciliation preserve record stamps.

use tokio_postgres::NoTls;
use wamn_control::{reconcile_run_plane, verification_policy::project_environment_policy};
use wamn_control_provision::PlatformComponent;
use wamn_schema_control::BareSchemaName;
use wamn_test_infrastructure::record_history::assert_stamp;

#[tokio::test]
async fn policy_projection_stamps_the_provisioning_actor() {
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    system.execute(&["BEGIN", &wamn_control_provision::bind_platform_principal_sql(PlatformComponent::Provisioning), "INSERT INTO registry.orgs(id,placement_type) VALUES ('history','dedicated'); INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('history','dev','\"own\"',0,1,'1Gi','1','1Gi','fixture');", "COMMIT"]).unwrap();
    let database = wamn_catalog::test_database::tenant();
    let (client, connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    let schema = BareSchemaName::new("wamn_run").unwrap();
    reconcile_run_plane::reconcile(&client, &schema, true)
        .await
        .unwrap();
    let first = project_environment_policy(
        system.url(),
        database.url(),
        &schema,
        "history",
        "history",
        "dev",
    )
    .await
    .unwrap();
    assert!(first.changed());
    let actor = PlatformComponent::Provisioning.principal_id().to_string();
    assert_stamp(
        &client,
        "wamn_run.environment_policies",
        "tenant_id='history'",
        &actor,
        &actor,
    )
    .await;
    let before: std::time::SystemTime = client
        .query_one(
            "SELECT updated_at FROM wamn_run.environment_policies WHERE tenant_id='history'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let second = project_environment_policy(
        system.url(),
        database.url(),
        &schema,
        "history",
        "history",
        "dev",
    )
    .await
    .unwrap();
    assert!(!second.changed());
    let after: std::time::SystemTime = client
        .query_one(
            "SELECT updated_at FROM wamn_run.environment_policies WHERE tenant_id='history'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(before, after);
    // The reconciler preserves the populated row while installing the same stamps.
    client.batch_execute("DROP TRIGGER wamn_record_history_stamp ON wamn_run.environment_policies; ALTER TABLE wamn_run.environment_policies DROP COLUMN created_at, DROP COLUMN created_by, DROP COLUMN updated_at, DROP COLUMN updated_by").await.unwrap();
    reconcile_run_plane::reconcile(&client, &schema, true)
        .await
        .unwrap();
    assert_stamp(
        &client,
        "wamn_run.environment_policies",
        "true",
        &actor,
        &actor,
    )
    .await;
    let plan = reconcile_run_plane::reconcile(&client, &schema, false)
        .await
        .unwrap();
    assert!(
        plan.actions.is_empty(),
        "fresh and migrated run plane disagree: {plan:?}"
    );
}

#[tokio::test]
async fn tenant_projection_and_instance_claim_stamp_provisioning() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let (mut client, connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    let triple = wamn_control_registry::Triple::new("history", "project", "dev");
    wamn_control::provision_project_env::project_tenant_environment(
        &mut client,
        &triple,
        Some("history"),
        "abcdefgh",
        false,
    )
    .await
    .unwrap();
    let actor = PlatformComponent::Provisioning.principal_id().to_string();
    assert_stamp(
        &client,
        "catalog.tenant_environments",
        "tenant_id='history'",
        &actor,
        &actor,
    )
    .await;
    let created: std::time::SystemTime = client
        .query_one(
            "SELECT created_at FROM catalog.tenant_environments WHERE tenant_id='history'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    wamn_control::provision_project_env::claim_environment_instance(
        &mut client,
        "history",
        "instance",
    )
    .await
    .unwrap();
    assert_stamp(
        &client,
        "catalog.tenant_environments",
        "tenant_id='history'",
        &actor,
        &actor,
    )
    .await;
    let row = client.query_one("SELECT created_at, environment_instance FROM catalog.tenant_environments WHERE tenant_id='history'", &[]).await.unwrap();
    assert_eq!(row.get::<_, std::time::SystemTime>(0), created);
    assert_eq!(row.get::<_, String>(1), "instance");
}
