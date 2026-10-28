//! Fresh system records require stamps and preserve the retirement actor.

use tokio_postgres::NoTls;
use wamn_control_provision::PlatformComponent;
use wamn_test_infrastructure::record_history::assert_stamp;

const RELATIONS: [&str; 11] = [
    "registry.meta",
    "registry.orgs",
    "registry.env_policies",
    "registry.projects",
    "registry.project_envs",
    "registry.retired_project_envs",
    "registry.capture_gap",
    "registry.generation_passwords",
    "identity.session_keys",
    "identity.session_signing_state",
    "identity.password_attempts",
];

#[tokio::test]
async fn fresh_system_history_requires_stamps_and_preserves_retirement_actor() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let (mut client, connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    let actor = PlatformComponent::Provisioning.principal_id().to_string();
    client
        .execute("SELECT set_config('app.user_id', $1, false)", &[&actor])
        .await
        .unwrap();
    client.batch_execute(
        "INSERT INTO registry.orgs(id,placement_type) VALUES ('history','dedicated'); \
         INSERT INTO registry.projects VALUES ('history','project'); \
         INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) \
           VALUES ('history','dev','{}',0,1,'1Gi','1','1Gi','fixture'); \
         INSERT INTO registry.project_envs(org,project,env,secret_name,instance_suffix,policy_name) \
           VALUES ('history','project','dev','secret','abcdefgh','dev'), \
                  ('history','project','retired','secret','ijklmnop','dev'); \
         DELETE FROM registry.project_envs WHERE env='retired'; \
         INSERT INTO registry.event_readers(org,project,env,publication,slot,stream,replication_secret_name) \
           VALUES ('history','project','dev','publication','slot','stream','secret'); \
         INSERT INTO registry.capture_gap(org,project,env,slot,start_at,reason,end_lsn) \
           VALUES ('history','project','dev','slot',now(),'fixture','0/10'); \
         INSERT INTO registry.generation_passwords(role,sha256) VALUES ('fixture',repeat('a',64)); \
         INSERT INTO identity.password_attempts(bucket,started_at,attempts) VALUES ('fixture',now(),1);"
    ).await.unwrap();
    wamn_platform_identity::session_keys::publish_session_key(
        &mut client,
        "https://history.invalid",
    )
    .await
    .unwrap();
    for relation in RELATIONS {
        assert_stamp(&client, relation, "true", &actor, &actor).await;
    }
    client.batch_execute("RESET app.user_id").await.unwrap();
    for relation in RELATIONS {
        let count: i64 = client
            .query_one(
                "SELECT count(*) FROM pg_attribute WHERE attrelid=$1::text::regclass \
             AND attname IN ('created_at','created_by','updated_at','updated_by') \
             AND attnotnull AND NOT atthasdef",
                &[&relation],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            count, 4,
            "{relation}: four required stamps without defaults"
        );
        let error = client
            .execute(&format!("UPDATE {relation} SET created_by=NULL"), &[])
            .await
            .unwrap_err();
        assert_eq!(
            error.code(),
            Some(&tokio_postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE),
            "{relation}: actor-required"
        );
    }
    // The production retirement trigger inherits the caller's actor on delete.
    let transaction = wamn_control::provision_project_env::provisioning_transaction(&mut client)
        .await
        .unwrap();
    transaction
        .execute("DELETE FROM registry.project_envs WHERE env='dev'", &[])
        .await
        .unwrap();
    assert_stamp(
        &transaction,
        "registry.retired_project_envs",
        "env='dev'",
        &actor,
        &actor,
    )
    .await;
    transaction.commit().await.unwrap();
    assert_eq!(
        client
            .execute("UPDATE registry.meta SET schema_version='unbound'", &[])
            .await
            .unwrap_err()
            .code(),
        Some(&tokio_postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
    );
}
