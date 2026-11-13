//! The policy writer and run-plane reconciliation preserve record stamps.

use sqlx::Row as _;
use wamn_control_provision::PlatformComponent;
use wamn_environment_control::reconcile_run_plane;
use wamn_environment_control::verification_policy::project_environment_policy;
use wamn_schema_control::BareSchemaName;
use wamn_test_infrastructure::record_history::assert_stamp;

#[tokio::test]
async fn policy_projection_stamps_the_provisioning_actor() {
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    system.execute(&["BEGIN", &wamn_control_provision::bind_platform_principal_sql(PlatformComponent::Provisioning), "INSERT INTO registry.orgs(id,placement_type) VALUES ('history','dedicated'); INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('history','dev','\"own\"',0,1,'1Gi','1','1Gi','fixture');", "COMMIT"]).unwrap();
    let database = wamn_catalog::test_database::project();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    let schema = BareSchemaName::new("wamn_run").unwrap();
    reconcile_run_plane::reconcile(&mut client, &schema, true)
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
        &mut client,
        "wamn_run.environment_policies",
        "project_env_key='history'",
        &actor,
        &actor,
    )
    .await;
    let before: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> = sqlx::query(
        "SELECT updated_at FROM wamn_run.environment_policies WHERE project_env_key='history'",
    )
    .fetch_one(&mut client)
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
    let after: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> = sqlx::query(
        "SELECT updated_at FROM wamn_run.environment_policies WHERE project_env_key='history'",
    )
    .fetch_one(&mut client)
    .await
    .unwrap()
    .get(0);
    assert_eq!(before, after);
    let plan = reconcile_run_plane::reconcile(&mut client, &schema, false)
        .await
        .unwrap();
    assert!(
        plan.actions.is_empty(),
        "current run plane requires repairs: {plan:?}"
    );
}
