//! Database privileges measured through the login and the PostgreSQL catalog.

#[path = "support/grants.rs"]
mod grants;
use wamn_control_provision::WorkloadRoleFamily;
#[path = "support/family_surface.rs"]
pub mod surface;
use surface::*;
use wamn_control_provision::sql;

/// THE CALLABLE-HTTP ADMITTER SURFACE, AS THE SERVER SEES IT.
///
/// Catalog reads, projected connection reads, permission reads, and no run plane
/// stay unchanged when provisioning removes an unauthorized privilege.
#[sqlx::test(migrations = false)]
async fn the_http_admitter_role_adds_exactly_the_fresh_permission_reads(pool: sqlx::PgPool) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let admin = test_database.as_str().to_owned();
    let database = query(&admin, "SELECT current_database()");
    reset(&admin, &database);

    let stable = WorkloadRoleFamily::HttpAdmitter.acl_role();
    let login = generation(WorkloadRoleFamily::HttpAdmitter, &database);
    let surface = sql::grant_http_admitter_surface_sql("wamn_run");
    run_admin(&admin, "converge the callable-HTTP surface", &surface);
    run_admin(&admin, "replay the convergent surface", &surface);
    run_admin(
        &admin,
        "prepare the callable-HTTP generation",
        &wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::HttpAdmitter,
            &database,
            &login,
            GENERATION_PW,
        ),
    );

    // Convergence, same as the executor arm: a run-plane privilege handed to
    // this family out of band is REMOVED by a re-apply, not merely unmentioned.
    run_admin(
        &admin,
        "widen the role onto adjacent authority",
        &format!(
            "GRANT USAGE ON SCHEMA wamn_run TO {stable};\n\
             GRANT SELECT ON TABLE wamn_run.runs TO {stable};\n\
             GRANT UPDATE (status) ON TABLE wamn_run.runs TO {stable};\n\
             GRANT SELECT ON TABLE platform.role TO {stable};\n"
        ),
    );
    run_admin(&admin, "re-converge after the widening", &surface);
    let sequences: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT n.nspname,c.relname,has_sequence_privilege($1,c.oid,'USAGE,SELECT,UPDATE')
         FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
         WHERE c.relkind='S' AND n.nspname NOT LIKE 'pg_%'",
    )
    .bind(&login)
    .fetch_all(&pool)
    .await
    .unwrap();
    for (schema, sequence, permitted) in sequences {
        assert!(!permitted, "reaches sequence {schema}.{sequence}");
    }
    let functions: Vec<(String, bool, bool)> = sqlx::query_as(
        "SELECT p.oid::regprocedure::text,has_function_privilege($1,p.oid,'EXECUTE'),
                EXISTS(SELECT FROM aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) a
                       WHERE a.grantee=0 AND a.privilege_type='EXECUTE') OR (n.nspname='wamn_authority' AND p.provolatile<>'v')
         FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
         WHERE n.nspname NOT LIKE 'pg_%' AND n.nspname<>'information_schema'",
    )
    .bind(&login)
    .fetch_all(&pool)
    .await
    .unwrap();
    for (function, permitted, public) in functions {
        assert!(
            !permitted || public,
            "routine authority permits writes: {function}"
        );
    }

    // --- THE POST-STATE, IN THE GENERATION'S OWN SESSION --------------------
    //
    // Every probe above is a privilege-catalog question, and the catalog is
    // BLIND TO RLS: `has_table_privilege` answers TRUE for a role whose every
    // SELECT returns zero rows because no policy matched it. All eight relations
    // carry FORCE RLS with a restrictive project base grants keyed on the GUEST login
    // pattern, so a non-guest family reads through the permissive
    // `TO wamn_platform` arm or it reads nothing at all — and reading nothing
    // RAISES NOTHING. This arm is therefore the only thing here that separates
    // an ISOLATED credential from a SILENTLY BLIND one (`wamn-0h0g.22.11`).
    //
    // Unlike the executor arm above, this one does NOT re-apply
    // `platform_group_membership_sql`. The edge is converged by
    // `test_database::login_sql` itself, through
    // `normalize_workload_generation_membership_sql`, so re-applying it here
    // would MASK the day that stops being true — which is precisely the
    // regression the counts below exist to catch.
    let digest = (format!("sha256:{}", "a".repeat(64)))
        .parse::<wamn_digest::Digest>()
        .unwrap();
    run_admin(
        &admin,
        "seed one readable fact in each catalog and fresh permission relation",
        &format!(
            "BEGIN;\n\
             INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('00000000-0000-4000-8000-000000000001'::text::uuid,'','fixture','admin:seed-family-surface-fixture');\n\
             INSERT INTO catalog.packages ( project_env_key,package_id,package_version,artifact_digest) VALUES ( 'org-a--prod--orders','orders','1.0.0','{digest}');\n\
             INSERT INTO catalog.releases (project_env_key,manifest_digest,canonical_bytes) \
             VALUES ('org-a--prod--orders','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','{{}}');\n\
             INSERT INTO platform.integration (id,integration_type,name,status) \
             VALUES ('00000000-0000-4000-8000-000000000011','http','upstream','active');\n\
             INSERT INTO platform.connection (id,integration_id,integration_type,status) \
             VALUES ('00000000-0000-4000-8000-000000000012','00000000-0000-4000-8000-000000000011','http','active');\n\
             INSERT INTO catalog.connection_requirement (project_env_key,component_digest,package_id,name,interface,mode) \
             VALUES ('org-a--prod--orders','{digest}','orders','upstream','wamn:connection/http@0.1.0','write');\n\
             INSERT INTO catalog.release_connection (project_env_key,manifest_digest,connection_id,version) \
             VALUES ('org-a--prod--orders','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','00000000-0000-4000-8000-000000000012',1);\n\
             INSERT INTO catalog.connection_binding (project_env_key,manifest_digest,component_digest,package_id,name,connection_id) \
             VALUES ('org-a--prod--orders','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','{digest}','orders','upstream','00000000-0000-4000-8000-000000000012');\n\
             INSERT INTO catalog.workflows \
               (project_env_key, environment, application, workflow_id, version, graph_json, \
                wiring_hash, trigger, execution_type, execution_role, created_by) \
             VALUES ('org-a--prod--orders', 'prod', 'orders', 'send', 1, '{{}}', '{digest}', \
                     'manual', 'role', 'widget-reader', \
                     '00000000-0000-4000-8000-000000000001');\n\
             INSERT INTO platform.role (name) \
             VALUES ('widget-reader');\n\
             INSERT INTO platform.\"user\" (id, type, email) \
             VALUES ('00000000-0000-4000-8000-000000000001', 'user', 'caller@example.test');\n\
             INSERT INTO platform.user_role (user_id, role_name) \
             VALUES ('00000000-0000-4000-8000-000000000001', 'widget-reader');\n\
             INSERT INTO platform.permission (role_name, permission) \
             VALUES ('widget-reader', 'wamn-widgets:widget/get');\n\
             COMMIT;\n"
        ),
    );

    let as_login = role_url(&admin, &login, GENERATION_PW);
    for statement in [
        "SELECT name FROM platform.role",
        "SELECT created_by FROM catalog.workflows",
    ] {
        assert_eq!(sqlstate(&as_login, statement).as_deref(), Some("42501"));
    }
    assert!(
        !bound_query(
            &as_login,
            "SELECT (rolsuper OR rolbypassrls)::text FROM pg_roles WHERE rolname = current_user"
        )
        .parse::<bool>()
        .unwrap(),
        "the reading session is superuser or bypasses RLS, which satisfies every \
         count below while establishing nothing"
    );
    let relations = query(
        &admin,
        &format!(
            "SELECT c.relname FROM pg_class c WHERE c.relnamespace='catalog'::regnamespace AND c.relkind IN ('r','p') AND has_table_privilege('{login}',c.oid,'SELECT')"
        ),
    );
    for relation in relations.lines() {
        assert_eq!(
            bound_query(
                &as_login,
                &format!("SELECT count(*) FROM catalog.{relation}")
            )
            .parse::<i64>()
            .unwrap(),
            1,
            "the callable-HTTP generation read the WRONG NUMBER of rows from \
             catalog.{relation}. Zero is the failure this arm exists to catch: the \
             row is seeded and the SELECT is granted, so zero means the project base grants \
             admitted nothing and the credential is isolated into blindness rather \
             than into authority"
        );
    }
    assert_eq!(
        bound_query(
            &as_login,
            "SELECT count(graph_json) FROM catalog.workflows \
              WHERE project_env_key = 'org-a--prod--orders' AND environment = 'prod' \
                AND application = 'orders' AND workflow_id = 'send' AND version = 1"
        )
        .parse::<i64>()
        .unwrap(),
        1,
        "the callable-HTTP generation cannot read the workflow graph through its \
         column grant, or the project base grants admitted nothing"
    );
    assert_eq!(
        bound_query(
            &as_login,
            "SELECT count(*) FROM platform.permission \
              WHERE role_name = 'widget-reader' \
                AND permission = 'wamn-widgets:widget/get'"
        )
        .parse::<i64>()
        .unwrap(),
        1,
        "the callable-HTTP generation cannot read the exact operation grant"
    );
    assert_eq!(
        bound_query(
            &as_login,
            "SELECT p.permission FROM platform.\"user\" u \
             JOIN platform.user_role ur ON ur.user_id = u.id \
             JOIN platform.permission p ON p.role_name = ur.role_name \
             WHERE u.id = '00000000-0000-4000-8000-000000000001' \
               AND u.status = 'active'"
        ),
        "wamn-widgets:widget/get",
        "the callable-HTTP generation cannot read the user's fresh permission"
    );
}

#[sqlx::test(migrations = false)]
async fn http_admitter_refusals_and_catalog_rules(pool: sqlx::PgPool) {
    grants::family(pool, WorkloadRoleFamily::HttpAdmitter).await;
}
