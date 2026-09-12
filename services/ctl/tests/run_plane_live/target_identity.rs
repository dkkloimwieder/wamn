use super::{
    CLI_ENV, CLI_INSTANCE, CLI_ORG, CLI_PROJECT, Client, RECONCILE_TARGET_REFUSAL_PREFIX,
    ReconcileRunPlaneArgs, ReconcileTargetError, ReconcileTargetErrorKind, SCHEMA, connect,
    database_url, drop_database, project_env_database_name, project_environment_policy,
    reconcile_run_plane, recreate_database, reset, schema, support,
};

async fn seed_target_guard_registry(su: &Client) {
    su.batch_execute(
        "DROP SCHEMA IF EXISTS registry CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') \
           THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$; \
         CREATE SCHEMA registry AUTHORIZATION wamn_system; \
         SET ROLE wamn_system; \
         CREATE TABLE registry.project_envs ( \
           org text NOT NULL, project text NOT NULL, env text NOT NULL, \
           secret_name text NOT NULL, secret_namespace text, \
           instance_suffix text NOT NULL, PRIMARY KEY (org, project, env)); \
         INSERT INTO registry.project_envs \
           (org,project,env,secret_name,instance_suffix) VALUES \
           ('acme','billing','dev','wamn-db-acme--billing--dev','k3m9x2p7'), \
           ('acme','ledger','dev','wamn-db-acme--ledger--dev','q80zdw41'), \
           ('acme','billing','prod','wamn-db-acme--billing--prod','p7c4n2v8'); \
         RESET ROLE; \
         CREATE TABLE registry.env_policies ( \
           org text NOT NULL, name text NOT NULL, recovery_domain jsonb NOT NULL, \
           promotion_rank int NOT NULL, instances int NOT NULL, storage text NOT NULL, \
           cpu text NOT NULL, memory text NOT NULL, image text NOT NULL, \
           backup_cadence text NOT NULL, wal_retention text NOT NULL, \
           hibernation text NOT NULL, PRIMARY KEY (org, name)); \
         INSERT INTO registry.env_policies \
           (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image, \
            backup_cadence,wal_retention,hibernation) VALUES \
           ('acme','dev','\"own\"',0,1,'1Gi','100m','128Mi','postgres','','','off'), \
           ('acme','prod','\"own\"',1,1,'1Gi','100m','128Mi','postgres','','','off')",
    )
    .await
    .expect("seed target-identity registry fixture");
}

async fn target_guard_system_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'policy-columns', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array( \
                       attribute.attname, \
                       pg_catalog.format_type(attribute.atttypid, attribute.atttypmod), \
                       attribute.attnotnull, \
                       pg_catalog.pg_get_expr(default_row.adbin, default_row.adrelid)) \
                      ORDER BY attribute.attnum) \
               FROM pg_catalog.pg_attribute AS attribute \
               LEFT JOIN pg_catalog.pg_attrdef AS default_row \
                 ON default_row.adrelid=attribute.attrelid \
                AND default_row.adnum=attribute.attnum \
              WHERE attribute.attrelid='registry.env_policies'::regclass \
                AND attribute.attnum > 0 AND NOT attribute.attisdropped), '[]'::jsonb), \
           'policy-owner', ( \
             SELECT pg_catalog.pg_get_userbyid(relation.relowner) \
               FROM pg_catalog.pg_class AS relation \
              WHERE relation.oid='registry.env_policies'::regclass), \
           'policies', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(org,name,recovery_domain,promotion_rank) \
                              ORDER BY org,name) \
               FROM registry.env_policies), '[]'::jsonb), \
           'project-envs', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array( \
                       org,project,env,secret_name,secret_namespace,instance_suffix) \
                      ORDER BY org,project,env) \
               FROM registry.project_envs), '[]'::jsonb), \
           'roles', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array( \
                       rolname,rolsuper,rolcreatedb,rolcreaterole,rolcanlogin,rolbypassrls) \
                      ORDER BY rolname) \
               FROM pg_catalog.pg_roles WHERE rolname LIKE 'wamn_%'), '[]'::jsonb), \
           'memberships', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(member.rolname,granted.rolname) \
                              ORDER BY member.rolname,granted.rolname) \
               FROM pg_catalog.pg_auth_members AS membership \
               JOIN pg_catalog.pg_roles AS member ON member.oid=membership.member \
               JOIN pg_catalog.pg_roles AS granted ON granted.oid=membership.roleid \
              WHERE member.rolname LIKE 'wamn_%' OR granted.rolname LIKE 'wamn_%'), \
             '[]'::jsonb))::text",
        &[],
    )
    .await
    .expect("snapshot registry target and cluster roles")
    .get(0)
}

async fn target_guard_database_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'database-acl', (SELECT datacl::text FROM pg_catalog.pg_database \
                             WHERE datname=current_database()), \
           'schemas', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(namespace.nspname, \
                                                pg_catalog.pg_get_userbyid(namespace.nspowner)) \
                              ORDER BY namespace.nspname) \
               FROM pg_catalog.pg_namespace AS namespace \
              WHERE namespace.nspname <> 'information_schema' \
                AND namespace.nspname NOT LIKE 'pg_%'), '[]'::jsonb), \
           'relations', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(namespace.nspname,relation.relname, \
                                                relation.relkind,relation.relrowsecurity, \
                                                relation.relforcerowsecurity) \
                              ORDER BY namespace.nspname,relation.relname) \
               FROM pg_catalog.pg_class AS relation \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid=relation.relnamespace \
              WHERE namespace.nspname <> 'information_schema' \
                AND namespace.nspname NOT LIKE 'pg_%'), '[]'::jsonb), \
           'policies', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(schemaname,tablename,policyname,roles,cmd,qual,with_check) \
                              ORDER BY schemaname,tablename,policyname) \
               FROM pg_catalog.pg_policies \
              WHERE schemaname <> 'information_schema' \
                AND schemaname NOT LIKE 'pg_%'), '[]'::jsonb))::text",
        &[],
    )
    .await
    .expect("snapshot project database target")
    .get(0)
}

fn target_guard_args(
    system_url: &str,
    target_url: &str,
    project: &str,
    environment: &str,
    dry_run: bool,
) -> ReconcileRunPlaneArgs {
    ReconcileRunPlaneArgs {
        system_database_url: system_url.to_string(),
        admin_database_url: target_url.to_string(),
        org: CLI_ORG.to_string(),
        project: project.to_string(),
        tenant: "t1".to_string(),
        env: environment.to_string(),
        schema: SCHEMA.to_string(),
        dry_run,
    }
}

/// The public CLI shell refuses every registry/database identity disagreement
/// before either the system-policy carrier or either project database changes.
#[tokio::test]
async fn reconcile_target_identity_guard_live() {
    let Some(system_url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the run-plane target-identity gate");
        return;
    };
    let system_su = connect(&system_url).await;
    let primary_database = project_env_database_name(CLI_ORG, CLI_PROJECT, CLI_ENV, CLI_INSTANCE);
    let sibling_project_database =
        project_env_database_name(CLI_ORG, "ledger", CLI_ENV, "q80zdw41");
    let sibling_environment_database =
        project_env_database_name(CLI_ORG, CLI_PROJECT, "prod", "p7c4n2v8");
    let unrelated_database = "wamn-run-plane-unrelated";
    for database in [primary_database.as_str(), unrelated_database] {
        recreate_database(&system_su, database).await;
    }
    seed_target_guard_registry(&system_su).await;

    let primary_url = database_url(&system_url, &primary_database);
    let unrelated_url = database_url(&system_url, unrelated_database);
    let primary_su = connect(&primary_url).await;
    let unrelated_su = connect(&unrelated_url).await;
    unrelated_su
        .batch_execute(&format!(
            "CREATE SCHEMA target_spoof; \
             CREATE FUNCTION target_spoof.current_database() RETURNS name \
               LANGUAGE sql IMMUTABLE AS $$ SELECT '{primary_database}'::name $$"
        ))
        .await
        .expect("install a search-path identity-spoof function");
    let mut spoofed_unrelated_url =
        url::Url::parse(&unrelated_url).expect("parse unrelated database URL");
    spoofed_unrelated_url
        .query_pairs_mut()
        .append_pair("options", "-csearch_path=target_spoof,pg_catalog");
    let spoofed_unrelated_url = spoofed_unrelated_url.to_string();
    let system_before = target_guard_system_snapshot(&system_su).await;
    let primary_before = target_guard_database_snapshot(&primary_su).await;
    let unrelated_before = target_guard_database_snapshot(&unrelated_su).await;

    let cases = [
        (
            "valid triple with unrelated URL",
            CLI_PROJECT,
            CLI_ENV,
            unrelated_url.as_str(),
            ReconcileTargetErrorKind::DatabaseTarget,
            Some(primary_database.as_str()),
            Some(unrelated_database),
        ),
        (
            "search-path spoofed unrelated URL",
            CLI_PROJECT,
            CLI_ENV,
            spoofed_unrelated_url.as_str(),
            ReconcileTargetErrorKind::DatabaseTarget,
            Some(primary_database.as_str()),
            Some(unrelated_database),
        ),
        (
            "registered sibling project with primary URL",
            "ledger",
            CLI_ENV,
            primary_url.as_str(),
            ReconcileTargetErrorKind::DatabaseTarget,
            Some(sibling_project_database.as_str()),
            Some(primary_database.as_str()),
        ),
        (
            "registered sibling environment with primary URL",
            CLI_PROJECT,
            "prod",
            primary_url.as_str(),
            ReconcileTargetErrorKind::DatabaseTarget,
            Some(sibling_environment_database.as_str()),
            Some(primary_database.as_str()),
        ),
        (
            "unrecorded triple",
            "absent",
            CLI_ENV,
            primary_url.as_str(),
            ReconcileTargetErrorKind::RegistryTarget,
            None,
            None,
        ),
    ];

    for dry_run in [true, false] {
        for &(label, project, environment, target_url, kind, expected, actual) in &cases {
            let error = reconcile_run_plane::run(target_guard_args(
                &system_url,
                target_url,
                project,
                environment,
                dry_run,
            ))
            .await
            .unwrap_err();
            let refusal = error
                .downcast_ref::<ReconcileTargetError>()
                .unwrap_or_else(|| {
                    panic!("{label} returned an untyped refusal with dry_run={dry_run}: {error}")
                });
            assert_eq!(refusal.kind(), kind, "{label}, dry_run={dry_run}");
            assert_eq!(
                refusal.is_registry_target(),
                kind == ReconcileTargetErrorKind::RegistryTarget,
                "{label}, dry_run={dry_run}"
            );
            assert_eq!(
                refusal.is_database_target(),
                kind == ReconcileTargetErrorKind::DatabaseTarget,
                "{label}, dry_run={dry_run}"
            );
            assert_eq!(
                refusal.expected_database(),
                expected,
                "{label}, dry_run={dry_run}"
            );
            assert_eq!(
                refusal.actual_database(),
                actual,
                "{label}, dry_run={dry_run}"
            );
            let message = refusal.to_string();
            assert!(
                message.starts_with(RECONCILE_TARGET_REFUSAL_PREFIX),
                "{label} lost the stable target-refusal prefix: {message}"
            );
            assert!(
                !message.contains(&*system_url) && !message.contains(target_url),
                "{label} leaked a database URL: {message}"
            );
            if kind == ReconcileTargetErrorKind::RegistryTarget {
                assert!(
                    std::error::Error::source(refusal).is_some(),
                    "{label} discarded the registry lookup source"
                );
            }
            assert_eq!(
                target_guard_system_snapshot(&system_su).await,
                system_before,
                "{label} mutated the pre-carrier registry or cluster roles with dry_run={dry_run}"
            );
            assert_eq!(
                target_guard_database_snapshot(&primary_su).await,
                primary_before,
                "{label} mutated the primary project database with dry_run={dry_run}"
            );
            assert_eq!(
                target_guard_database_snapshot(&unrelated_su).await,
                unrelated_before,
                "{label} mutated the unrelated database with dry_run={dry_run}"
            );
        }
    }

    assert!(
        primary_su
            .query_one("SELECT to_regnamespace($1) IS NULL", &[&SCHEMA])
            .await
            .expect("probe refused primary schema")
            .get::<_, bool>(0),
        "wrong-target attempts created the run-plane schema"
    );
    system_su
        .batch_execute("ALTER TABLE registry.env_policies OWNER TO wamn_system")
        .await
        .expect("make the policy fixture readable for the correct-target path");
    reset(&primary_su).await;

    let system_before_dry_run = target_guard_system_snapshot(&system_su).await;
    let primary_before_dry_run = target_guard_database_snapshot(&primary_su).await;
    reconcile_run_plane::run(target_guard_args(
        &system_url,
        &primary_url,
        CLI_PROJECT,
        CLI_ENV,
        true,
    ))
    .await
    .expect("correct target dry-run plans without writing");
    assert_eq!(
        target_guard_system_snapshot(&system_su).await,
        system_before_dry_run,
        "correct-target dry-run changed the system plane"
    );
    assert_eq!(
        target_guard_database_snapshot(&primary_su).await,
        primary_before_dry_run,
        "correct-target dry-run changed the project plane"
    );

    reconcile_run_plane::run(target_guard_args(
        &system_url,
        &primary_url,
        CLI_PROJECT,
        CLI_ENV,
        false,
    ))
    .await
    .expect("correct target apply converges the run plane");
    assert!(
        primary_su
            .query_one(
                "SELECT to_regclass($1) IS NOT NULL",
                &[&format!("{SCHEMA}.runs")],
            )
            .await
            .expect("probe converged run plane")
            .get::<_, bool>(0),
        "correct target did not converge the run plane"
    );
    let projected_row = primary_su
        .query_one(
            &format!(
                "SELECT expected_environment,durability_class, \
                        source_policy_org,source_policy_hash \
                   FROM {SCHEMA}.environment_policies WHERE tenant_id='t1'"
            ),
            &[],
        )
        .await
        .expect("read converged project-local policy");
    let expected_source_hash = wamn_execution_contract::canonical_json_sha256(&serde_json::json!({
        "backup-cadence": "",
        "cpu": "100m",
        "durability-class": "standard",
        "hibernation": "off",
        "image": "postgres",
        "instances": 1,
        "memory": "128Mi",
        "name": CLI_ENV,
        "promotion-rank": 0,
        "recovery-domain": "own",
        "storage": "1Gi",
        "wal-retention": "",
    }));
    let projected = (
        projected_row.get::<_, String>(0),
        projected_row.get::<_, String>(1),
        projected_row.get::<_, Option<String>>(2),
        projected_row.get::<_, Option<String>>(3),
    );
    assert_eq!(
        projected,
        (
            CLI_ENV.to_string(),
            "standard".to_string(),
            Some(CLI_ORG.to_string()),
            Some(expected_source_hash.clone()),
        )
    );
    let replay =
        project_environment_policy(&system_url, &primary_url, &schema(), CLI_ORG, "t1", CLI_ENV)
            .await
            .expect("replay the exact source policy projection");
    assert!(!replay.changed(), "exact projection replay was not a no-op");
    assert_eq!(replay.source_policy_org(), CLI_ORG);
    assert_eq!(replay.environment(), CLI_ENV);
    assert_eq!(replay.source_policy_hash(), expected_source_hash);
    primary_su
        .execute(
            &format!(
                "UPDATE {SCHEMA}.environment_policies \
                    SET source_policy_hash=$1 WHERE tenant_id='t1'"
            ),
            &[&format!("sha256:{}", "0".repeat(64))],
        )
        .await
        .expect("seed a stale projected policy hash");
    assert!(
        project_environment_policy(&system_url, &primary_url, &schema(), CLI_ORG, "t1", CLI_ENV,)
            .await
            .expect("repair the stale policy projection")
            .changed(),
        "stale projection was not reported as changed"
    );
    assert!(
        !project_environment_policy(&system_url, &primary_url, &schema(), CLI_ORG, "t1", CLI_ENV,)
            .await
            .expect("replay the repaired policy projection")
            .changed(),
        "repaired projection did not converge"
    );
    let carrier_present: bool = system_su
        .query_one(
            "SELECT EXISTS (SELECT FROM information_schema.columns \
              WHERE table_schema='registry' AND table_name='env_policies' \
                AND column_name='durability_class')",
            &[],
        )
        .await
        .expect("probe converged system durability carrier")
        .get(0);
    assert!(
        carrier_present,
        "correct apply did not converge the policy carrier"
    );

    let system_converged = target_guard_system_snapshot(&system_su).await;
    let primary_converged = target_guard_database_snapshot(&primary_su).await;
    reconcile_run_plane::run(target_guard_args(
        &system_url,
        &primary_url,
        CLI_PROJECT,
        CLI_ENV,
        false,
    ))
    .await
    .expect("correct target replay remains converged");
    assert_eq!(
        target_guard_system_snapshot(&system_su).await,
        system_converged
    );
    assert_eq!(
        target_guard_database_snapshot(&primary_su).await,
        primary_converged
    );
    assert_eq!(
        target_guard_database_snapshot(&unrelated_su).await,
        unrelated_before,
        "correct-target convergence touched the unrelated database"
    );

    drop(primary_su);
    drop(unrelated_su);
    for database in [primary_database.as_str(), unrelated_database] {
        drop_database(&system_su, database).await;
    }
}
