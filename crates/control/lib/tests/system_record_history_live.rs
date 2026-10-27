//! Fresh and upgraded system stamps agree, including populated backfills.

use tokio_postgres::{Client, NoTls};
use wamn_control_provision::{PlatformComponent, schema_migrations::SYSTEM_MIGRATIONS};
use wamn_test_infrastructure::record_history::assert_stamp;

const RELATIONS: [(&str, &str); 11] = [
    ("registry.meta", "transaction_timestamp()"),
    ("registry.orgs", "transaction_timestamp()"),
    ("registry.env_policies", "transaction_timestamp()"),
    ("registry.projects", "transaction_timestamp()"),
    ("registry.project_envs", "transaction_timestamp()"),
    ("registry.retired_project_envs", "retired_at"),
    ("registry.capture_gap", "created_at"),
    ("registry.generation_passwords", "recorded_at"),
    ("identity.session_keys", "published_at"),
    ("identity.session_signing_state", "transaction_timestamp()"),
    ("identity.password_attempts", "started_at"),
];

// Fresh bootstrap appends target_id through 0031 after the core schema.
// Compare business-column order and stamp-column order separately.
async fn shape(client: &Client) -> Vec<String> {
    let names: Vec<_> = RELATIONS.iter().map(|(name, _)| name.to_string()).collect();
    client
        .query(
            "WITH relations AS (SELECT unnest($1::text[])::regclass AS oid) \
         SELECT entry FROM (\
           SELECT a.attrelid::regclass || ' column ' || \
             row_number() OVER (PARTITION BY a.attrelid, \
               a.attname IN ('created_at','created_by','updated_at','updated_by') ORDER BY a.attnum) || ' ' || a.attname || ' ' || \
             format_type(a.atttypid,a.atttypmod) || ' ' || a.attnotnull || ' ' || \
             coalesce(pg_get_expr(d.adbin,d.adrelid),'') AS entry \
           FROM pg_attribute a JOIN relations r ON r.oid=a.attrelid \
           LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum \
           WHERE a.attnum>0 AND NOT a.attisdropped \
           UNION ALL SELECT conrelid::regclass || ' constraint ' || conname || ' ' || \
             pg_get_constraintdef(c.oid) FROM pg_constraint c JOIN relations r ON r.oid=c.conrelid \
           UNION ALL SELECT tgrelid::regclass || ' trigger ' || pg_get_triggerdef(t.oid) \
           FROM pg_trigger t JOIN relations r ON r.oid=t.tgrelid WHERE NOT t.tgisinternal \
         ) entries ORDER BY entry",
            &[&names],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn system_history_migration_matches_fresh_schema_and_backfills_existing_rows() {
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
    let fresh = shape(&client).await;
    // Restore the exact unstamped table shapes from before migration 0032.
    for (relation, history_time) in RELATIONS {
        client
            .batch_execute(&format!(
                "DROP TRIGGER wamn_record_history_stamp ON {relation};"
            ))
            .await
            .unwrap();
        let created = if relation == "registry.capture_gap" {
            "ALTER COLUMN created_at SET DEFAULT now()"
        } else {
            "DROP COLUMN created_at"
        };
        client.batch_execute(&format!("ALTER TABLE {relation} {created}, DROP COLUMN created_by, DROP COLUMN updated_at, DROP COLUMN updated_by")).await.unwrap();
        if history_time != "transaction_timestamp()" {
            client
                .batch_execute(&format!(
                    "UPDATE {relation} SET {history_time} = '2020-01-02T03:04:05Z'"
                ))
                .await
                .unwrap();
        }
    }
    client.batch_execute("RESET app.user_id").await.unwrap();
    let transaction = client.transaction().await.unwrap();
    let migration = SYSTEM_MIGRATIONS
        .iter()
        .find(|migration| migration.ordinal() == 32)
        .expect("the system record history migration exists");
    transaction.batch_execute(migration.sql).await.unwrap();
    for (relation, history_time) in RELATIONS {
        assert_stamp(&transaction, relation, "true", &actor, &actor).await;
        let row = transaction.query_one(&format!(
            "SELECT bool_and(created_at = {history_time} AND updated_at = created_at) FROM {relation}"
        ), &[]).await.unwrap();
        assert!(row.get::<_, bool>(0), "{relation}: backfill time");
    }
    transaction.commit().await.unwrap();
    assert_eq!(shape(&client).await, fresh);
    for (relation, _) in RELATIONS {
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
