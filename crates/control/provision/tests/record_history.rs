//! Fresh schemas stamp records and production writers bind their actor.

use wamn_control_provision::{
    CONTROL_BOOTSTRAP_SQL, PlatformComponent, bind_platform_principal_sql,
};

#[sqlx::test(migrations = false)]
async fn fresh_run_history_stamps_rows(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    {
        let batches: &[&str] = &[
            &wamn_control_provision::sql::ensure_app_acl_role_sql(),
            wamn_catalog::CATALOG_SCHEMA_SQL,
            include_str!("../../../../deploy/sql/run-state.sql"),
            include_str!("../../../../deploy/sql/run-queue.sql"),
        ];
        let mut setup_connection = wamn_postgres::connect(database.as_str()).await.unwrap();
        for batch in batches {
            sqlx::raw_sql(sqlx::AssertSqlSafe(*batch))
                .execute(&mut setup_connection)
                .await
                .unwrap();
        }
    }
    {
        let batches: &[&str] = &[
            "BEGIN",
            &bind_platform_principal_sql(PlatformComponent::Provisioning),
            r"
        INSERT INTO catalog.releases (project_env_key,manifest_digest,canonical_bytes) VALUES ('history','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','{}') ON CONFLICT DO NOTHING; INSERT INTO wamn_run.runs (project_env_key, run_id, wiring_id, wiring_version, wiring_hash, package_id, environment, trigger_source, execution_principal_id, execution_role, invoked_by, invoked_by_user_id, node_digests_json, manifest_digest) VALUES ('history', 'run', 'fixture', 1, 'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd', 'package', 'dev', 'manual', '00000000-0000-0000-0000-000000000001', 'admin', 'manual', '00000000-0000-0000-0000-000000000001', '{}', 'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a');
    ",
            "INSERT INTO wamn_run.run_queue(project_env_key,run_id) VALUES ('history','run');",
            "COMMIT",
        ];
        let mut setup_connection = wamn_postgres::connect(database.as_str()).await.unwrap();
        for batch in batches {
            sqlx::raw_sql(sqlx::AssertSqlSafe(*batch))
                .execute(&mut setup_connection)
                .await
                .unwrap();
        }
    }
    let actor = PlatformComponent::Provisioning.principal_id();
    for relation in ["wamn_run.run_queue"] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DO $$ BEGIN ASSERT (SELECT count(*)=1 AND bool_and(created_by='{actor}'::uuid AND updated_by='{actor}'::uuid AND created_at=updated_at AND isfinite(created_at)) FROM {relation}), '{relation}: stamps'; END $$;"))).execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap()).await.unwrap();
    }
}

#[sqlx::test(migrations = false)]
async fn fresh_control_history_stamps_production_writes(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    {
        let batches: &[&str] = &[
            "DO $$ BEGIN IF to_regrole('wamn_system') IS NULL THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$;",
            wamn_control_provision::sql::ensure_db_owner_role_sql(),
            "SET ROLE wamn_system",
            CONTROL_BOOTSTRAP_SQL[0],
            CONTROL_BOOTSTRAP_SQL[1],
            "BEGIN",
            &bind_platform_principal_sql(PlatformComponent::Provisioning),
            &format!(
                "PREPARE mapping(text,text,text,text,text) AS {}; EXECUTE mapping('production-login','org--dev--project','org','project','dev');",
                wamn_control_provision::sql::upsert_control_author_project_env_key_mapping_sql()
            ),
            "COMMIT",
        ];
        let mut setup_connection = wamn_postgres::connect(database.as_str()).await.unwrap();
        for batch in batches {
            sqlx::raw_sql(sqlx::AssertSqlSafe(*batch))
                .execute(&mut setup_connection)
                .await
                .unwrap();
        }
    }
    let actor = PlatformComponent::Provisioning.principal_id();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DO $$ BEGIN ASSERT EXISTS (SELECT FROM wamn_authority.author_login_projects WHERE login_identity='production-login' AND created_by='{actor}'::uuid AND updated_by='{actor}'::uuid AND created_at=updated_at AND isfinite(created_at)); END $$;"))).execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap()).await.unwrap();
}
