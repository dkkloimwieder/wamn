//! PG18 lifecycle proof for private effect-writer credential generations.
//!
//! Run only against a disposable cluster: the test creates one database and
//! cluster-global roles, revokes PUBLIC CONNECT on every non-template database,
//! and revokes PUBLIC TEMPORARY on the exact project database.
//!
//! `WAMN_EFFECT_WRITER_PG18_URL=postgres://.../postgres cargo test -p wamn-ctl \
//!   --test effect_writer_generation_live -- --ignored --nocapture`

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tokio_postgres::{Client, NoTls};
use url::Url;

use wamn_control_provision::{
    CredentialGeneration, EFFECT_WRITER_CREDENTIAL_KEY, EFFECT_WRITER_ROLE,
    EffectWriterCredentialScope, effect_writer_generation_role, project_env_database_name, sql,
};
use wamn_ctl::provision_project_env::{self, ProvisionProjectEnvArgs};
use wamn_run_state::RUN_PROJECTION_WRITER_ROLE;

const ORG: &str = "pg18proof";
const PROJECT: &str = "ledger";
const ENVIRONMENT: &str = "dev";
const LEDGER_SCHEMA: &str = "wamn_runner_demo";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect disposable PG18");
    tokio::spawn(connection);
    client
}

fn database_url(admin_url: &str, database: &str) -> String {
    let mut url = Url::parse(admin_url).expect("parse PG18 admin URL");
    url.set_path(&format!("/{database}"));
    url.set_query(None);
    url.set_fragment(None);
    url.into()
}

fn secret_path(generation: CredentialGeneration) -> PathBuf {
    std::env::temp_dir().join(format!(
        "wamn-effect-writer-pg18-{}-{}.json",
        std::process::id(),
        generation.as_str()
    ))
}

fn is_scoped_generation_role(role: &str) -> bool {
    let Some(scoped) = role.strip_prefix("wamn_effect_writer_") else {
        return false;
    };
    let Some((scope_hash, generation)) = scoped.rsplit_once('_') else {
        return false;
    };
    scope_hash.len() == 40
        && scope_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        && matches!(generation, "a" | "b")
}

fn action_args(
    target_admin_url: &str,
    prepare: Option<(CredentialGeneration, &Path)>,
    retire: Option<CredentialGeneration>,
    abort: Option<CredentialGeneration>,
) -> ProvisionProjectEnvArgs {
    ProvisionProjectEnvArgs {
        org: Some(ORG.to_string()),
        project: Some(PROJECT.to_string()),
        env: Some(ENVIRONMENT.to_string()),
        system_database_url: None,
        cluster: None,
        connection_limit: None,
        app_password: "unused".to_string(),
        app_host: None,
        app_port: 5432,
        namespace: "wamn-system".to_string(),
        secret_namespace: None,
        target_admin_database_url: Some(target_admin_url.to_string()),
        prepare_effect_writer_generation: prepare.map(|(generation, _)| generation),
        retire_effect_writer_generation: retire,
        abort_effect_writer_generation: abort,
        emit_effect_writer_secret: prepare.map(|(_, path)| path.to_path_buf()),
        artifact_reader_tenant_id: None,
        prepare_artifact_reader_generation: None,
        retire_artifact_reader_generation: None,
        revoke_artifact_reader_credential: false,
        emit_artifact_reader_secret: None,
        emit_database: None,
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: None,
        emit_management_author_pat_secret: None,
        emit_route_caller_pat_secret: None,
        revoke_pat_prefix: None,
    }
}

fn credential_document(path: &Path, expected: &EffectWriterCredentialScope) -> (String, String) {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("read emitted Secret"))
            .expect("parse emitted Secret");
    let document = manifest["stringData"][EFFECT_WRITER_CREDENTIAL_KEY]
        .as_str()
        .expect("credential.json stringData");
    let credential = wamn_control_provision::parse_effect_writer_credential(document.as_bytes())
        .expect("strict credential document");
    wamn_control_provision::validate_effect_writer_credential(
        &credential,
        expected,
        std::time::SystemTime::now(),
    )
    .expect("fresh exact-scope credential");
    let document: serde_json::Value = serde_json::from_str(document).expect("credential JSON");
    (
        document["url"]
            .as_str()
            .expect("credential URL")
            .to_string(),
        document["expires-at"]
            .as_str()
            .expect("credential expires-at")
            .to_string(),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the role attributes, validity, membership, and database ACL are independent security assertions"
)]
async fn assert_role(
    admin: &Client,
    role: &str,
    login: bool,
    inherit: bool,
    password_set: bool,
    valid_until: Option<&str>,
    memberships: &[&str],
    connect_databases: &[&str],
) {
    let row = admin
        .query_one(sql::effect_writer_generation_state_sql(), &[&role])
        .await
        .expect("read exact role state");
    assert_eq!(row.get::<_, bool>("rolcanlogin"), login);
    assert!(!row.get::<_, bool>("rolsuper"));
    assert_eq!(row.get::<_, bool>("rolinherit"), inherit);
    assert!(!row.get::<_, bool>("rolcreaterole"));
    assert!(!row.get::<_, bool>("rolcreatedb"));
    assert!(!row.get::<_, bool>("rolreplication"));
    assert!(!row.get::<_, bool>("rolbypassrls"));
    assert_eq!(row.get::<_, bool>("password_set"), password_set);
    assert_eq!(
        row.get::<_, Option<String>>("valid_until").as_deref(),
        valid_until
    );
    assert_eq!(
        row.get::<_, bool>("valid_until_finite"),
        valid_until.is_some()
    );
    assert_eq!(
        row.get::<_, Vec<String>>("memberships"),
        memberships
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>()
    );
    assert!(row.get::<_, bool>("membership_options_exact"));
    let member_roles: Vec<String> = row.get("member_roles");
    assert!(row.get::<_, bool>("member_options_exact"));
    assert!(row.get::<_, bool>("generation_children_exact"));
    if [EFFECT_WRITER_ROLE, RUN_PROJECTION_WRITER_ROLE].contains(&role) {
        assert!(
            member_roles
                .iter()
                .all(|member| is_scoped_generation_role(member)),
            "stable ACL role has an unexpected member"
        );
    } else {
        assert!(member_roles.is_empty());
    }
    assert_eq!(
        row.get::<_, Vec<String>>("connect_databases"),
        connect_databases
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(row.get::<_, i64>("owned_objects"), 0);
    if !login {
        assert_eq!(row.get::<_, i64>("sessions"), 0);
    }
}

async fn direct_acl_set(admin: &Client, role: &str) -> BTreeSet<String> {
    admin
        .query(sql::role_database_acl_inventory_sql(), &[&role])
        .await
        .expect("read direct ACL inventory")
        .into_iter()
        .map(|row| {
            format!(
                "{}:{}:{}:{}",
                row.get::<_, String>("object_kind"),
                row.get::<_, String>("schema_name"),
                row.get::<_, String>("object_name"),
                row.get::<_, String>("privilege_type")
            )
        })
        .collect()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL 18 via WAMN_EFFECT_WRITER_PG18_URL"]
async fn effect_writer_generation_lifecycle_is_exact_and_fail_closed() {
    let admin_url = std::env::var("WAMN_EFFECT_WRITER_PG18_URL")
        .expect("set WAMN_EFFECT_WRITER_PG18_URL to a disposable PG18 superuser URL");
    let catalog = connect(&admin_url).await;
    let version: i32 = catalog
        .query_one("SHOW server_version_num", &[])
        .await
        .expect("read PG version")
        .get::<_, String>(0)
        .parse()
        .expect("numeric PG version");
    assert!(
        version >= 180_000,
        "credential proof requires PostgreSQL 18"
    );

    let database = project_env_database_name(ORG, PROJECT, ENVIRONMENT);
    let role_a = effect_writer_generation_role(
        ORG,
        PROJECT,
        ENVIRONMENT,
        &database,
        CredentialGeneration::A,
    );
    let role_b = effect_writer_generation_role(
        ORG,
        PROJECT,
        ENVIRONMENT,
        &database,
        CredentialGeneration::B,
    );
    catalog
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS \"{database}\" WITH (FORCE)"
        ))
        .await
        .expect("drop prior lifecycle database");
    catalog
        .batch_execute(&format!(
            "DROP ROLE IF EXISTS \"{role_a}\"; DROP ROLE IF EXISTS \"{role_b}\"; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
               THEN CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; END IF; END $$;"
        ))
        .await
        .expect("reset lifecycle roles");
    catalog
        .batch_execute(&format!("CREATE DATABASE \"{database}\""))
        .await
        .expect("create lifecycle database");
    let public_connect_before: bool = catalog
        .query_one(
            "SELECT has_database_privilege('public', $1::text, 'CONNECT')",
            &[&database],
        )
        .await
        .expect("probe new lifecycle database PUBLIC CONNECT")
        .get(0);
    assert!(public_connect_before);
    catalog
        .batch_execute(&sql::grant_connect_on_database_sql(&database))
        .await
        .expect("revoke target PUBLIC TEMPORARY and grant app CONNECT");

    let target_url = database_url(&admin_url, &database);
    let target = connect(&target_url).await;
    target
        .batch_execute(
            "CREATE SCHEMA wamn_runner_demo; CREATE SCHEMA wamn_system; CREATE SCHEMA catalog; \
             CREATE SCHEMA app; CREATE SCHEMA unrelated; \
             CREATE TABLE wamn_runner_demo.effect_attempts (id bigint); \
             CREATE TABLE wamn_runner_demo.effect_attempt_dispatches (id bigint); \
             CREATE TABLE wamn_runner_demo.effect_attempt_outcomes (id bigint); \
             CREATE TABLE wamn_runner_demo.runs ( \
               tenant_id text, run_id text, status text, flow_id text); \
             CREATE TABLE wamn_runner_demo.run_queue ( \
               tenant_id text, run_id text, lease_owner text, \
               lease_expires_at timestamptz, lease_generation bigint); \
             CREATE TABLE wamn_runner_demo.node_runs (tenant_id text, run_id text); \
             CREATE TABLE wamn_system.probe (id bigint); CREATE TABLE catalog.probe (id bigint); \
             CREATE TABLE app.probe (id bigint); CREATE TABLE unrelated.probe (id bigint);",
        )
        .await
        .expect("create privilege fixtures");
    let scope = EffectWriterCredentialScope {
        org: ORG.to_string(),
        project: PROJECT.to_string(),
        environment: ENVIRONMENT.to_string(),
        database: database.clone(),
    };
    let secret_a = secret_path(CredentialGeneration::A);
    let secret_b = secret_path(CredentialGeneration::B);
    provision_project_env::run(action_args(
        &target_url,
        Some((CredentialGeneration::A, &secret_a)),
        None,
        None,
    ))
    .await
    .expect("prepare initial A");
    assert!(
        target
            .query(sql::public_connect_databases_sql(), &[])
            .await
            .expect("ctl converged PUBLIC CONNECT floor")
            .is_empty()
    );
    target
        .batch_execute(
            "GRANT USAGE ON SCHEMA wamn_runner_demo TO wamn_effect_writer; \
             GRANT SELECT, INSERT ON wamn_runner_demo.effect_attempts, \
               wamn_runner_demo.effect_attempt_dispatches, \
               wamn_runner_demo.effect_attempt_outcomes \
               TO wamn_effect_writer; \
             GRANT SELECT (tenant_id,run_id,status) \
               ON wamn_runner_demo.runs TO wamn_effect_writer; \
             GRANT SELECT (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
               ON wamn_runner_demo.run_queue TO wamn_effect_writer; \
             GRANT USAGE ON SCHEMA wamn_runner_demo TO wamn_run_projection_writer; \
             GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_runner_demo.node_runs \
               TO wamn_run_projection_writer;",
        )
        .await
        .expect("apply schema-control-owned ledger grants");

    let (url_a, expires_a) = credential_document(&secret_a, &scope);
    let (client_a, connection_a) = tokio_postgres::connect(&url_a, NoTls)
        .await
        .expect("authenticate A from emitted Secret");
    let connection_a = tokio::spawn(connection_a);
    assert_role(
        &target,
        EFFECT_WRITER_ROLE,
        false,
        false,
        false,
        None,
        &[],
        &[],
    )
    .await;
    assert_role(
        &target,
        RUN_PROJECTION_WRITER_ROLE,
        false,
        false,
        false,
        None,
        &[],
        &[],
    )
    .await;
    assert_role(
        &target,
        &role_a,
        true,
        true,
        true,
        Some(&expires_a),
        &[EFFECT_WRITER_ROLE, RUN_PROJECTION_WRITER_ROLE],
        &[&database],
    )
    .await;
    assert_eq!(
        direct_acl_set(&target, &role_a).await,
        BTreeSet::from([format!("database:{database}:{database}:CONNECT")])
    );
    let stable_acl = direct_acl_set(&target, EFFECT_WRITER_ROLE).await;
    let mut expected_stable =
        BTreeSet::from([format!("schema:{LEDGER_SCHEMA}:{LEDGER_SCHEMA}:USAGE")]);
    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ] {
        for privilege in ["INSERT", "SELECT"] {
            expected_stable.insert(format!("relation:{LEDGER_SCHEMA}:{table}:{privilege}"));
        }
    }
    for (table, columns) in [
        ("runs", &["tenant_id", "run_id", "status"][..]),
        (
            "run_queue",
            &[
                "tenant_id",
                "run_id",
                "lease_owner",
                "lease_expires_at",
                "lease_generation",
            ][..],
        ),
    ] {
        for column in columns {
            expected_stable.insert(format!("column:{LEDGER_SCHEMA}:{table}.{column}:SELECT"));
        }
    }
    assert_eq!(stable_acl, expected_stable);
    assert_eq!(
        direct_acl_set(&target, RUN_PROJECTION_WRITER_ROLE).await,
        BTreeSet::from([
            format!("schema:{LEDGER_SCHEMA}:{LEDGER_SCHEMA}:USAGE"),
            format!("relation:{LEDGER_SCHEMA}:node_runs:DELETE"),
            format!("relation:{LEDGER_SCHEMA}:node_runs:INSERT"),
            format!("relation:{LEDGER_SCHEMA}:node_runs:SELECT"),
            format!("relation:{LEDGER_SCHEMA}:node_runs:UPDATE"),
        ])
    );
    let exact_run_reads = client_a
        .query_one(
            "SELECT \
               NOT has_table_privilege(current_user,'wamn_runner_demo.runs','SELECT'), \
               has_column_privilege(current_user,'wamn_runner_demo.runs','status','SELECT'), \
               NOT has_column_privilege(current_user,'wamn_runner_demo.runs','flow_id','SELECT'), \
               NOT has_table_privilege(current_user,'wamn_runner_demo.run_queue','SELECT'), \
               has_column_privilege(current_user,'wamn_runner_demo.run_queue', \
                                    'lease_expires_at','SELECT'), \
               has_column_privilege(current_user,'wamn_runner_demo.run_queue', \
                                    'lease_generation','SELECT'), \
               NOT has_any_column_privilege(current_user,'wamn_runner_demo.run_queue', \
                                            'INSERT,UPDATE,REFERENCES')",
            &[],
        )
        .await
        .expect("probe exact inherited writer run-read boundary");
    for index in 0..7 {
        assert!(
            exact_run_reads.get::<_, bool>(index),
            "run-read ACL probe {index}"
        );
    }
    let unrelated_connect: bool = target
        .query_one(
            "SELECT has_database_privilege($1, 'postgres', 'CONNECT')",
            &[&role_a],
        )
        .await
        .expect("probe unrelated database CONNECT")
        .get(0);
    assert!(!unrelated_connect);
    assert!(
        target
            .query(sql::public_connect_databases_sql(), &[])
            .await
            .expect("verify PUBLIC floor")
            .is_empty()
    );
    let public_temporary: bool = target
        .query_one(sql::public_temporary_on_current_database_sql(), &[])
        .await
        .expect("verify target PUBLIC TEMPORARY floor")
        .get(0);
    assert!(!public_temporary);
    let generation_temporary: bool = client_a
        .query_one(
            "SELECT has_database_privilege(current_user, current_database(), 'TEMPORARY')",
            &[],
        )
        .await
        .expect("probe generation TEMPORARY")
        .get(0);
    assert!(!generation_temporary);
    let temporary_error = client_a
        .batch_execute("CREATE TEMPORARY TABLE effect_writer_shadow_probe (id bigint)")
        .await
        .expect_err("effect-writer generation created a temporary relation");
    assert_eq!(
        temporary_error.code(),
        Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
    );
    for schema in ["wamn_system", "catalog", "app", "unrelated"] {
        let allowed: bool = client_a
            .query_one(
                "SELECT has_schema_privilege(current_user, $1, 'USAGE')",
                &[&schema],
            )
            .await
            .expect("probe non-ledger schema privilege")
            .get(0);
        assert!(!allowed, "generation unexpectedly has USAGE on {schema}");
        assert!(
            client_a
                .simple_query(&format!("SELECT * FROM {schema}.probe"))
                .await
                .is_err(),
            "generation read non-ledger object in {schema}"
        );
    }
    let app_probe = connect(&target_url).await;
    app_probe
        .batch_execute("SET ROLE wamn_app")
        .await
        .expect("assume ordinary app role");
    assert!(
        app_probe
            .simple_query("INSERT INTO wamn_runner_demo.effect_attempts VALUES (1)")
            .await
            .is_err(),
        "wamn_app wrote the private ledger"
    );

    provision_project_env::run(action_args(
        &target_url,
        Some((CredentialGeneration::B, &secret_b)),
        None,
        None,
    ))
    .await
    .expect("prepare unpublished B");
    provision_project_env::run(action_args(
        &target_url,
        None,
        None,
        Some(CredentialGeneration::B),
    ))
    .await
    .expect("abort unpublished B");
    assert_role(
        &target,
        &role_b,
        false,
        true,
        false,
        Some("1970-01-01T00:00:00Z"),
        &[],
        &[],
    )
    .await;
    let abort_published = provision_project_env::run(action_args(
        &target_url,
        None,
        None,
        Some(CredentialGeneration::A),
    ))
    .await
    .expect_err("abort accepted the published in-use generation");
    assert!(abort_published.to_string().contains("cannot be aborted"));
    provision_project_env::run(action_args(
        &target_url,
        Some((CredentialGeneration::B, &secret_b)),
        None,
        None,
    ))
    .await
    .expect("prepare B overlap");
    let generation_roles = vec![role_a.clone(), role_b.clone()];
    let login_overlap: i64 = target
        .query_one(
            "SELECT count(*) FROM pg_roles WHERE rolname = ANY($1) AND rolcanlogin",
            &[&generation_roles],
        )
        .await
        .expect("count overlap")
        .get(0);
    assert_eq!(login_overlap, 2);
    let (url_b, expires_b) = credential_document(&secret_b, &scope);
    let (client_b, connection_b) = tokio_postgres::connect(&url_b, NoTls)
        .await
        .expect("authenticate B from emitted Secret");
    let connection_b = tokio::spawn(connection_b);
    provision_project_env::run(action_args(
        &target_url,
        None,
        Some(CredentialGeneration::A),
        None,
    ))
    .await
    .expect("retire A after B use");
    assert!(client_a.simple_query("SELECT 1").await.is_err());
    let _ = connection_a.await;
    assert_role(
        &target,
        &role_a,
        false,
        true,
        false,
        Some("1970-01-01T00:00:00Z"),
        &[],
        &[],
    )
    .await;
    assert_role(
        &target,
        &role_b,
        true,
        true,
        true,
        Some(&expires_b),
        &[EFFECT_WRITER_ROLE, RUN_PROJECTION_WRITER_ROLE],
        &[&database],
    )
    .await;
    assert_eq!(
        direct_acl_set(&target, &role_b).await,
        BTreeSet::from([format!("database:{database}:{database}:CONNECT")])
    );
    assert!(direct_acl_set(&target, &role_a).await.is_empty());
    let login_steady: i64 = target
        .query_one(
            "SELECT count(*) FROM pg_roles WHERE rolname = ANY($1) AND rolcanlogin",
            &[&generation_roles],
        )
        .await
        .expect("count steady state")
        .get(0);
    assert_eq!(login_steady, 1);

    drop(client_b);
    let _ = connection_b.await;
    drop(target);
    catalog
        .batch_execute(&format!("DROP DATABASE \"{database}\" WITH (FORCE)"))
        .await
        .expect("drop lifecycle database");
    catalog
        .batch_execute(&format!("DROP ROLE \"{role_a}\"; DROP ROLE \"{role_b}\";"))
        .await
        .expect("clean scoped lifecycle roles while retaining stable ACL roles");
    let _ = std::fs::remove_file(secret_a);
    let _ = std::fs::remove_file(secret_b);
}
