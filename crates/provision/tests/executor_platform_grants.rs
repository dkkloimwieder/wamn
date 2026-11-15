//! Database privileges measured through the login and the PostgreSQL catalog.

#[path = "support/grants.rs"]
mod grants;
use wamn_control_provision::WorkloadRoleFamily;
#[path = "support/family_surface.rs"]
pub mod surface;
use surface::*;
use wamn_control_provision::sql;

/// THE EXECUTOR-PLATFORM SURFACE, AS THE SERVER SEES IT.
#[sqlx::test(migrations = false)]
async fn the_executor_platform_role_holds_exactly_its_measured_lease_surface(pool: sqlx::PgPool) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let admin = test_database.as_str().to_owned();
    let database = query(&admin, "SELECT current_database()");
    reset(&admin, &database);

    let stable = WorkloadRoleFamily::ExecutorPlatform.acl_role();
    let login = generation(WorkloadRoleFamily::ExecutorPlatform, &database);
    // The convergent grant batch, applied TWICE: a replay against an already
    // correct database must be a no-op, not a widening and not an error.
    let surface = sql::grant_executor_platform_surface_sql("wamn_run");
    run_admin(&admin, "converge the executor-platform surface", &surface);
    run_admin(&admin, "replay the convergent surface", &surface);
    run_admin(
        &admin,
        "prepare the executor-platform generation",
        &wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::ExecutorPlatform,
            &database,
            &login,
            GENERATION_PW,
        ),
    );
    run_admin(
        &admin,
        "member the family into the platform base grants arm",
        &sql::platform_group_membership_sql(WorkloadRoleFamily::ExecutorPlatform),
    );

    let expected = load_role_grants(&admin, stable).await;
    let direct: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (
          SELECT FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) a WHERE a.grantee=$1::text::regrole
          UNION ALL SELECT FROM pg_attribute c CROSS JOIN LATERAL aclexplode(c.attacl) a WHERE a.grantee=$1::text::regrole
          UNION ALL SELECT FROM pg_namespace c CROSS JOIN LATERAL aclexplode(c.nspacl) a WHERE a.grantee=$1::text::regrole
          UNION ALL SELECT FROM pg_proc c CROSS JOIN LATERAL aclexplode(c.proacl) a WHERE a.grantee=$1::text::regrole
          UNION ALL SELECT FROM pg_database c CROSS JOIN LATERAL aclexplode(c.datacl) a WHERE a.grantee=$1::text::regrole
            AND (c.datname<>current_database() OR a.privilege_type<>'CONNECT' OR a.is_grantable))"
    ).bind(&login).fetch_one(&pool).await.unwrap();
    assert!(
        direct,
        "the login has only CONNECT directly and inherits its operation privileges"
    );

    // --- 2. CONVERGENCE: a widening applied out of band is REMOVED ----------
    run_admin(
        &admin,
        "widen the role out of band",
        &format!(
            "GRANT INSERT ON TABLE wamn_run.runs TO {stable};\n\
             GRANT UPDATE (input_json) ON TABLE wamn_run.runs TO {stable};\n\
             GRANT SELECT ON TABLE catalog.wiring_tombstones TO {stable};\n\
             GRANT EXECUTE ON FUNCTION \
               wamn_run.guard_event_lineage_immutable() TO {stable};\n"
        ),
    );
    run_admin(&admin, "re-converge after the widening", &surface);
    assert_eq!(
        load_role_grants(&admin, stable).await,
        expected,
        "the surface ADDS but does not NARROW: a hand-granted table, column and \
         routine privilege survived a re-apply"
    );

    // --- 4. THE POST-STATE, IN THE GENERATION'S OWN SESSION -----------------
    //
    // A privilege catalog says what PostgreSQL believes; this says what it does.
    // Both directions are read back, because under FORCE RLS a statement that
    // matched nothing and a statement that was refused look identical from the
    // outside.
    run_admin(
        &admin,
        "seed one run the executor may lease",
        &format!(
            "BEGIN; {actor} INSERT INTO catalog.packages ( project_env_key, package_id, package_version, artifact_digest) VALUES ( 't1--dev--project', 'widgets', '1.0.0', 'sha256:{package_hash}');\n\
             \n\
             INSERT INTO catalog.releases (project_env_key,manifest_digest,canonical_bytes) VALUES ('t1--dev--project','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','{{}}') ON CONFLICT DO NOTHING; INSERT INTO wamn_run.runs (project_env_key, run_id, package_id, environment, status, trigger_source, wiring_id, wiring_version, wiring_hash, input_json, execution_principal_id, execution_role, invoked_by, invoked_by_user_id, node_digests_json, manifest_digest) VALUES ('t1--dev--project', 'r1', 'widgets', 'dev', 'dispatched', 'manual', 'w', 1, 'sha256:{hash}', '{{\"a\":1}}', '00000000-0000-0000-0000-000000000001', 'admin', 'manual', '00000000-0000-0000-0000-000000000001', '{{}}', 'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'); COMMIT;\n",
            actor = wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning
            ),
            hash = "c".repeat(64),
            package_hash = "d".repeat(64),
        ),
    );
    let as_login = role_url(&admin, &login, GENERATION_PW);
    assert_eq!(
        sqlstate(
            &as_login,
            &format!(
                "BEGIN; {} UPDATE wamn_run.runs SET status = 'running' WHERE run_id = 'r1'; COMMIT;",
                wamn_control_provision::bind_platform_principal_sql(
                    wamn_control_provision::PlatformComponent::Executor
                )
            ),
        ),
        None,
        "a granted column must be writable by the generation itself"
    );
    assert_eq!(
        query(
            &admin,
            "SELECT status FROM wamn_run.runs WHERE run_id = 'r1'"
        ),
        "running",
        "the granted UPDATE reported success and moved nothing: the post-state is \
         the only thing that separates a write from a no-op under FORCE RLS"
    );
    assert_eq!(
        sqlstate(
            &as_login,
            &format!("BEGIN; {} UPDATE wamn_run.runs SET input_json = '{{\"b\":2}}' WHERE run_id = 'r1'; COMMIT;", wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Executor)),
        )
        .as_deref(),
        Some("42501"),
        "the executor rewrote the authoritative input: column-grain UPDATE is the \
         only bound available, because the permissive platform base grants arm is \
         USING (true) and reaches every project"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&query(
            &admin,
            "SELECT input_json FROM wamn_run.runs WHERE run_id='r1'"
        ))
        .unwrap()["a"],
        1
    );
}

#[sqlx::test(migrations = false)]
async fn executor_platform_refusals_and_catalog_rules(pool: sqlx::PgPool) {
    grants::family(pool, WorkloadRoleFamily::ExecutorPlatform).await;
}
