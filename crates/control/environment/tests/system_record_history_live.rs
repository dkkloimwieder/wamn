//! Fresh system records require stamps and preserve the retirement actor.

use wamn_control_provision::PlatformComponent;
use wamn_test_infrastructure::record_history::assert_stamp;

#[tokio::test]
async fn fresh_system_history_requires_stamps_and_preserves_retirement_actor() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    let actor = PlatformComponent::Provisioning.principal_id().to_string();
    sqlx::query("SELECT set_config('app.user_id', $1, false)")
        .bind(&actor)
        .execute(&mut client)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    sqlx::raw_sql("INSERT INTO registry.orgs(id,placement_type) VALUES ('history','dedicated'); \
         INSERT INTO registry.projects VALUES ('history','project'); \
         INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) \
           VALUES ('history','dev','{}',0,1,'1Gi','1','1Gi','fixture'); \
         WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ('history','dev','dev'),('history','retired','dev') ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ('history','project','dev','secret','abcdefgh'),('history','project','retired','secret','ijklmnop'); \
         DELETE FROM registry.environment_projects WHERE env='retired'; \
         INSERT INTO registry.event_readers(org,project,env,publication,slot,stream,replication_secret_name) \
           VALUES ('history','project','dev','publication','slot','stream','secret'); \
         INSERT INTO registry.capture_gap(org,project,env,slot,start_at,reason,end_lsn) \
           VALUES ('history','project','dev','slot',now(),'fixture','0/10'); \
         INSERT INTO identity.password_attempts(bucket,started_at,attempts) VALUES ('fixture',now(),1);").execute(&mut client).await.map(drop).unwrap();
    wamn_platform_identity::session_keys::publish_session_key(
        &mut client,
        "https://history.invalid",
    )
    .await
    .unwrap();
    let relations:Vec<String>=sqlx::query_scalar("SELECT format('%I.%I',n.nspname,c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN ('registry','identity') AND EXISTS(SELECT FROM pg_trigger t WHERE t.tgrelid=c.oid AND t.tgfoid='wamn_history.stamp_row()'::regprocedure)")
        .fetch_all(&mut client).await.unwrap();
    let mut seeded = Vec::new();
    for relation in relations {
        let present: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT EXISTS(SELECT FROM {relation} WHERE created_by::text=$1)"
        )))
        .bind(&actor)
        .fetch_one(&mut client)
        .await
        .unwrap();
        if present {
            assert_stamp(
                &mut client,
                &relation,
                &format!("created_by::text='{actor}'"),
                &actor,
                &actor,
            )
            .await;
            seeded.push(relation);
        }
    }
    sqlx::raw_sql("RESET app.user_id")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    for relation in seeded {
        let error = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {relation} SET created_by=NULL WHERE created_by::text='{actor}'"
        )))
        .execute(&mut client)
        .await
        .map(|result| result.rows_affected())
        .unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("55000"),
            "{relation}: actor-required"
        );
    }
    // The production retirement trigger inherits the caller's actor on delete.
    let mut transaction =
        wamn_environment_control::provision_project_env::provisioning_transaction(&mut client)
            .await
            .unwrap();
    sqlx::query("DELETE FROM registry.environment_projects WHERE env='dev'")
        .execute(&mut *transaction)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    assert_stamp(
        &mut transaction,
        "registry.retired_project_envs",
        "env='dev'",
        &actor,
        &actor,
    )
    .await;
    transaction.commit().await.unwrap();
    assert_eq!(
        sqlx::query("UPDATE registry.meta SET platform_domain='unbound.example'")
            .execute(&mut client)
            .await
            .map(|result| result.rows_affected())
            .unwrap_err()
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("55000")
    );
}
