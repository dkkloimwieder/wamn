//! PostgreSQL 18 lifecycle proof for the control artifact-reader credential.
//!
//! Run only against a disposable cluster: the test creates one database and
//! cluster-global roles and revokes PUBLIC CONNECT on every non-template
//! database.
//!
//! `WAMN_ARTIFACT_READER_PG18_URL=postgres://.../postgres cargo test -p wamn-ctl \
//!   --test artifact_reader_generation_live -- --ignored --nocapture`

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;

use tokio_postgres::error::SqlState;
use tokio_postgres::{Client, Config, NoTls};
use url::Url;

use wamn_control_provision::{
    ARTIFACT_READER_APPLICATION_NAME, ARTIFACT_READER_CREDENTIAL_KEY,
    ArtifactReaderCredentialScope, ArtifactReaderTenantScope, CONTROL_BOOTSTRAP_SQL,
    CredentialGeneration, artifact_reader_endpoint, artifact_reader_generation_role,
    artifact_reader_policy_name, artifact_reader_secret_name, artifact_reader_tenant_role,
    parse_artifact_reader_credential, sql, validate_artifact_reader_credential,
    validate_artifact_reader_generation_role_marker, validate_artifact_reader_tenant_role_marker,
};
use wamn_ctl::provision_project_env::{self, ProvisionProjectEnvArgs};

const ORG: &str = "pg18proof";
const PROJECT: &str = "artifact-reader";
const ENVIRONMENT: &str = "dev";
const TENANT: &str = "tenant-a";
const FOREIGN_TENANT: &str = "tenant-b";
const DATABASE: &str = "wamn_artifact_reader_pg18";
const COLLISION_ROLE: &str = "artifact_reader_policy_collision";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect disposable PostgreSQL 18");
    tokio::spawn(connection);
    client
}

async fn connect_reader(
    url: &str,
    application_name: &str,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
) {
    let mut config = Config::from_str(url).expect("parse artifact-reader credential URL");
    config.application_name(application_name);
    let (client, connection) = config
        .connect(NoTls)
        .await
        .expect("authenticate artifact-reader generation");
    (client, tokio::spawn(connection))
}

fn database_url(admin_url: &str, database: &str) -> String {
    let mut url = Url::parse(admin_url).expect("parse PostgreSQL 18 admin URL");
    url.set_path(&format!("/{database}"));
    url.set_query(None);
    url.set_fragment(None);
    url.into()
}

fn secret_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "wamn-artifact-reader-pg18-{}.json",
        std::process::id()
    ))
}

fn blocked_secret_path() -> (PathBuf, PathBuf) {
    let parent = std::env::temp_dir().join(format!(
        "wamn-artifact-reader-pg18-blocked-{}",
        std::process::id()
    ));
    (parent.join("credential.json"), parent)
}

fn scope() -> ArtifactReaderCredentialScope {
    ArtifactReaderCredentialScope {
        tenant_id: TENANT.to_string(),
        org: ORG.to_string(),
        project: PROJECT.to_string(),
        environment: ENVIRONMENT.to_string(),
        database: DATABASE.to_string(),
    }
}

fn action_args(
    admin_url: &str,
    prepare: Option<CredentialGeneration>,
    retire: Option<CredentialGeneration>,
    revoke: bool,
    secret: Option<&Path>,
) -> ProvisionProjectEnvArgs {
    ProvisionProjectEnvArgs {
        org: Some(ORG.to_string()),
        project: Some(PROJECT.to_string()),
        env: Some(ENVIRONMENT.to_string()),
        system_database_url: Some(admin_url.to_string()),
        cluster: None,
        connection_limit: None,
        app_password: "wamn_app".to_string(),
        app_host: None,
        app_port: 5432,
        namespace: "wamn-system".to_string(),
        secret_namespace: None,
        target_admin_database_url: None,
        prepare_effect_writer_generation: None,
        retire_effect_writer_generation: None,
        abort_effect_writer_generation: None,
        emit_effect_writer_secret: None,
        artifact_reader_tenant_id: Some(TENANT.to_string()),
        prepare_artifact_reader_generation: prepare,
        retire_artifact_reader_generation: retire,
        revoke_artifact_reader_credential: revoke,
        emit_artifact_reader_secret: secret.map(Path::to_path_buf),
        emit_database: None,
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: None,
        emit_management_author_pat_secret: None,
        emit_route_caller_pat_secret: None,
        revoke_pat_prefix: None,
    }
}

fn credential_document(
    path: &Path,
    admin_url: &str,
    expected_generation: CredentialGeneration,
) -> (String, String) {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("read emitted Secret"))
            .expect("parse emitted Secret");
    assert_eq!(
        manifest["metadata"]["name"],
        artifact_reader_secret_name(TENANT, ORG, PROJECT, ENVIRONMENT, DATABASE)
    );
    assert_eq!(
        manifest["metadata"]["annotations"]["wamn.io/credential-generation"],
        expected_generation.as_str()
    );
    let data = manifest["stringData"]
        .as_object()
        .expect("Secret stringData object");
    assert_eq!(data.len(), 1);
    let document = data[ARTIFACT_READER_CREDENTIAL_KEY]
        .as_str()
        .expect("credential.json stringData");
    let credential = parse_artifact_reader_credential(document.as_bytes())
        .expect("strict artifact-reader credential");
    let expected = scope();
    let endpoint = artifact_reader_endpoint(admin_url).expect("trusted control endpoint");
    validate_artifact_reader_credential(&credential, &expected, &endpoint, SystemTime::now())
        .expect("fresh exact-scope artifact-reader credential");
    assert_eq!(credential.generation(), expected_generation);
    assert_eq!(credential.tenant_id(), TENANT);
    assert_eq!(credential.database(), DATABASE);
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

async fn direct_acl_set(admin: &Client, role: &str) -> BTreeSet<String> {
    admin
        .query(sql::role_database_acl_inventory_sql(), &[&role])
        .await
        .expect("read direct role ACL inventory")
        .into_iter()
        .map(|row| {
            format!(
                "{}:{}:{}:{}:{}",
                row.get::<_, String>("object_kind"),
                row.get::<_, String>("schema_name"),
                row.get::<_, String>("object_name"),
                row.get::<_, String>("privilege_type"),
                row.get::<_, bool>("is_grantable")
            )
        })
        .collect()
}

async fn assert_policy(admin: &Client, stable_role: &str, generation_role: Option<&str>) {
    let policy = artifact_reader_policy_name(TENANT, DATABASE);
    let generation_role = generation_role.unwrap_or("");
    let rows = admin
        .query(
            sql::artifact_reader_policy_state_sql(),
            &[&stable_role, &generation_role, &policy],
        )
        .await
        .expect("read complete artifact-reader policy set");
    let actual = rows
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, bool>(2),
                row.get::<_, Vec<String>>(3),
                row.get::<_, Option<String>>(4),
                row.get::<_, Option<String>>(5),
            )
        })
        .collect::<Vec<_>>();
    let generic_qual = Some(
        "(tenant_id = NULLIF(current_setting('app.tenant'::text, true), ''::text))".to_string(),
    );
    let mut expected = vec![
        (
            "execution_bundles_tenant".to_string(),
            "*".to_string(),
            true,
            vec!["PUBLIC".to_string()],
            generic_qual.clone(),
            generic_qual,
        ),
        (
            policy,
            "r".to_string(),
            false,
            vec![stable_role.to_string()],
            Some("(tenant_id = 'tenant-a'::text)".to_string()),
            None,
        ),
    ];
    expected.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(actual, expected);
    let rls = admin
        .query_one(
            "SELECT relrowsecurity, relforcerowsecurity FROM pg_class \
             WHERE oid='catalog.execution_bundles'::regclass",
            &[],
        )
        .await
        .expect("read execution-bundle RLS state");
    assert!(rls.get::<_, bool>(0));
    assert!(rls.get::<_, bool>(1));
}

async fn assert_stable_role(admin: &Client, stable_role: &str, children: &[&str]) {
    let row = admin
        .query_one(
            sql::artifact_reader_tenant_role_state_sql(),
            &[&stable_role],
        )
        .await
        .expect("read stable artifact-reader role");
    assert!(!row.get::<_, bool>("rolcanlogin"));
    assert!(!row.get::<_, bool>("rolsuper"));
    assert!(!row.get::<_, bool>("rolinherit"));
    assert!(!row.get::<_, bool>("rolcreaterole"));
    assert!(!row.get::<_, bool>("rolcreatedb"));
    assert!(!row.get::<_, bool>("rolreplication"));
    assert!(!row.get::<_, bool>("rolbypassrls"));
    assert!(!row.get::<_, bool>("password_set"));
    assert_eq!(row.get::<_, Option<String>>("valid_until"), None);
    assert!(!row.get::<_, bool>("valid_until_finite"));
    assert_eq!(
        row.get::<_, Vec<String>>("memberships"),
        Vec::<String>::new()
    );
    assert!(row.get::<_, bool>("membership_options_exact"));
    assert_eq!(
        row.get::<_, Vec<String>>("member_roles"),
        children
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>()
    );
    assert!(row.get::<_, bool>("member_options_exact"));
    assert_eq!(
        row.get::<_, Vec<String>>("database_settings"),
        Vec::<String>::new()
    );
    assert_eq!(
        row.get::<_, Vec<String>>("connect_databases"),
        Vec::<String>::new()
    );
    assert_eq!(
        row.get::<_, Vec<String>>("effective_connect_databases"),
        Vec::<String>::new()
    );
    assert_eq!(row.get::<_, i64>("sessions"), 0);
    assert_eq!(row.get::<_, i64>("owned_objects"), 0);
    let marker: String = row
        .get::<_, Option<String>>("marker")
        .expect("stable role owner marker");
    validate_artifact_reader_tenant_role_marker(
        &marker,
        &ArtifactReaderTenantScope {
            tenant_id: TENANT.to_string(),
            database: DATABASE.to_string(),
        },
    )
    .expect("exact stable role marker");

    let mut expected = BTreeSet::new();
    for column in [
        "tenant_id",
        "execution_bundle_hash",
        "format_version",
        "exact_bytes",
        "byte_length",
    ] {
        expected.insert(format!(
            "column:catalog:execution_bundles.{column}:SELECT:false"
        ));
    }
    expected.insert("schema:catalog:catalog:USAGE:false".to_string());
    assert_eq!(direct_acl_set(admin, stable_role).await, expected);
    assert!(
        admin
            .query(sql::artifact_reader_public_authority_sql(), &[])
            .await
            .expect("read PUBLIC artifact-reader authority floor")
            .is_empty()
    );
    assert_policy(admin, stable_role, None).await;
}

async fn assert_generation_role(
    admin: &Client,
    role: &str,
    stable_role: &str,
    generation: CredentialGeneration,
    expires_at: Option<&str>,
) {
    let row = admin
        .query_one(sql::artifact_reader_generation_state_sql(), &[&role])
        .await
        .expect("read artifact-reader generation role");
    let active = expires_at.is_some();
    assert_eq!(row.get::<_, bool>("rolcanlogin"), active);
    assert!(!row.get::<_, bool>("rolsuper"));
    assert!(row.get::<_, bool>("rolinherit"));
    assert!(!row.get::<_, bool>("rolcreaterole"));
    assert!(!row.get::<_, bool>("rolcreatedb"));
    assert!(!row.get::<_, bool>("rolreplication"));
    assert!(!row.get::<_, bool>("rolbypassrls"));
    assert_eq!(row.get::<_, bool>("password_set"), active);
    assert_eq!(
        row.get::<_, Option<String>>("valid_until").as_deref(),
        expires_at.or(Some("1970-01-01T00:00:00Z"))
    );
    assert!(row.get::<_, bool>("valid_until_finite"));
    assert_eq!(
        row.get::<_, Vec<String>>("memberships"),
        if active {
            vec![stable_role.to_string()]
        } else {
            Vec::new()
        }
    );
    assert!(row.get::<_, bool>("membership_options_exact"));
    assert_eq!(
        row.get::<_, Vec<String>>("member_roles"),
        Vec::<String>::new()
    );
    assert!(row.get::<_, bool>("member_options_exact"));
    assert_eq!(
        row.get::<_, Vec<String>>("database_settings"),
        if active {
            vec![format!("{DATABASE}:app.tenant={TENANT}")]
        } else {
            Vec::new()
        }
    );
    assert_eq!(
        row.get::<_, Vec<String>>("connect_databases"),
        if active {
            vec![DATABASE.to_string()]
        } else {
            Vec::new()
        }
    );
    assert_eq!(
        row.get::<_, Vec<String>>("effective_connect_databases"),
        if active {
            vec![DATABASE.to_string()]
        } else {
            Vec::new()
        }
    );
    assert_eq!(row.get::<_, i64>("owned_objects"), 0);
    if !active {
        assert_eq!(row.get::<_, i64>("sessions"), 0);
    }
    let marker: String = row
        .get::<_, Option<String>>("marker")
        .expect("generation owner marker");
    validate_artifact_reader_generation_role_marker(&marker, &scope(), generation)
        .expect("exact generation role marker");
    assert_eq!(
        direct_acl_set(admin, role).await,
        if active {
            BTreeSet::from([format!("database:{DATABASE}:{DATABASE}:CONNECT:false")])
        } else {
            BTreeSet::new()
        }
    );
    assert_policy(admin, stable_role, Some(role)).await;
}

async fn assert_prepare_b_refused(
    admin_url: &str,
    target: &Client,
    secret: &Path,
    secret_a: &[u8],
    role_b: &str,
    boundary: &str,
    error_fragment: &str,
) {
    let error = provision_project_env::run(action_args(
        admin_url,
        Some(CredentialGeneration::B),
        None,
        false,
        Some(secret),
    ))
    .await
    .expect_err(boundary);
    let detail = format!("{error:#}");
    assert!(
        detail.contains(error_fragment),
        "{boundary}: unexpected error: {detail}"
    );
    assert_eq!(
        std::fs::read(secret).expect("read unchanged A Secret"),
        secret_a
    );
    assert!(
        target
            .query_opt(sql::artifact_reader_generation_state_sql(), &[&role_b])
            .await
            .expect("probe refused B generation")
            .is_none(),
        "{boundary}: B role survived a refused prepare"
    );
}

async fn assert_insufficient_privilege(client: &Client, sql: &str, boundary: &str) {
    let error = client.batch_execute(sql).await.expect_err(boundary);
    assert_eq!(
        error.code(),
        Some(&SqlState::INSUFFICIENT_PRIVILEGE),
        "{boundary}: {error}"
    );
}

async fn assert_reader_authority(client: &Client, stable_role: &str) {
    let rows = client
        .query(
            "SELECT tenant_id, execution_bundle_hash, format_version, exact_bytes, byte_length \
             FROM catalog.execution_bundles ORDER BY execution_bundle_hash",
            &[],
        )
        .await
        .expect("read exact five-column own-tenant projection");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, String>(0), TENANT);
    assert!(rows[0].get::<_, String>(1).starts_with("sha256:"));
    assert_eq!(rows[0].get::<_, String>(2), "0.1");
    assert_eq!(rows[0].get::<_, Vec<u8>>(3), b"own");
    assert_eq!(rows[0].get::<_, i32>(4), 3);

    client
        .query_one(
            "SELECT set_config('app.tenant', $1, false)",
            &[&FOREIGN_TENANT],
        )
        .await
        .expect("change caller-controlled tenant setting");
    let visible: i64 = client
        .query_one(
            "SELECT count(*)::bigint FROM catalog.execution_bundles",
            &[],
        )
        .await
        .expect("probe cross-tenant visibility")
        .get(0);
    assert_eq!(visible, 0);
    client
        .query_one("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("restore credential tenant setting");

    assert_insufficient_privilege(
        client,
        "SELECT created_at FROM catalog.execution_bundles",
        "created_at read",
    )
    .await;
    assert_insufficient_privilege(
        client,
        "SELECT * FROM catalog.execution_bundles",
        "table-wide execution-bundle read",
    )
    .await;
    assert_insufficient_privilege(
        client,
        "INSERT INTO catalog.execution_bundles \
         (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
         VALUES ('tenant-a','sha256:'||repeat('0',64),'0.1','x',1)",
        "execution-bundle insert",
    )
    .await;
    assert_insufficient_privilege(
        client,
        "UPDATE catalog.execution_bundles SET byte_length=byte_length",
        "execution-bundle update",
    )
    .await;
    assert_insufficient_privilege(
        client,
        "DELETE FROM catalog.execution_bundles",
        "execution-bundle delete",
    )
    .await;
    for (query, boundary) in [
        ("SELECT * FROM catalog.catalogs", "catalog relation read"),
        ("SELECT * FROM catalog.flow_drafts", "draft read"),
        (
            "SELECT * FROM catalog.release_flow_test_evidence",
            "release evidence read",
        ),
        (
            "SELECT * FROM catalog.deployment_attestations",
            "deployment attestation read",
        ),
        ("SELECT * FROM identity.principals", "principal read"),
        (
            "SELECT * FROM wamn_run.authoring_test_reports",
            "authoring report read",
        ),
    ] {
        assert_insufficient_privilege(client, query, boundary).await;
    }
    assert_insufficient_privilege(
        client,
        "CREATE TABLE catalog.artifact_reader_escape (id bigint)",
        "catalog DDL",
    )
    .await;
    assert_insufficient_privilege(
        client,
        &format!("SET ROLE \"{stable_role}\""),
        "SET ROLE escalation",
    )
    .await;
    assert_insufficient_privilege(
        client,
        "CREATE ROLE artifact_reader_escape",
        "role creation",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL 18 via WAMN_ARTIFACT_READER_PG18_URL"]
async fn artifact_reader_generation_lifecycle_is_exact_and_fail_closed() {
    let admin_url = std::env::var("WAMN_ARTIFACT_READER_PG18_URL")
        .expect("set WAMN_ARTIFACT_READER_PG18_URL to a disposable PG18 superuser URL");
    let catalog = connect(&admin_url).await;
    let version: i32 = catalog
        .query_one("SHOW server_version_num", &[])
        .await
        .expect("read PostgreSQL version")
        .get::<_, String>(0)
        .parse()
        .expect("numeric PostgreSQL version");
    assert!(
        version >= 180_000,
        "credential proof requires PostgreSQL 18"
    );

    let stable_role = artifact_reader_tenant_role(TENANT, DATABASE);
    let role_a = artifact_reader_generation_role(
        TENANT,
        ORG,
        PROJECT,
        ENVIRONMENT,
        DATABASE,
        CredentialGeneration::A,
    );
    let role_b = artifact_reader_generation_role(
        TENANT,
        ORG,
        PROJECT,
        ENVIRONMENT,
        DATABASE,
        CredentialGeneration::B,
    );
    let secret = secret_path();
    let (blocked_secret, blocked_parent) = blocked_secret_path();
    let _ = std::fs::remove_file(&secret);
    let _ = std::fs::remove_file(&blocked_parent);

    catalog
        .batch_execute(
            "REVOKE CREATE ON TABLESPACE pg_default FROM PUBLIC; \
             REVOKE SET ON PARAMETER application_name FROM PUBLIC",
        )
        .await
        .expect("clear exact cluster-wide authority-probe residue");
    catalog
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS \"{DATABASE}\" WITH (FORCE)"
        ))
        .await
        .expect("drop disposable control database");
    catalog
        .batch_execute(&format!(
            "DROP ROLE IF EXISTS \"{role_a}\"; \
             DROP ROLE IF EXISTS \"{role_b}\"; \
             DROP ROLE IF EXISTS \"{stable_role}\"; \
             DROP ROLE IF EXISTS \"{COLLISION_ROLE}\"; \
             DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
                 CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$;"
        ))
        .await
        .expect("reset disposable control roles");
    catalog
        .batch_execute(&format!("CREATE DATABASE \"{DATABASE}\" OWNER wamn_system"))
        .await
        .expect("create disposable control database");

    let target_url = database_url(&admin_url, DATABASE);
    let target = connect(&target_url).await;
    target
        .batch_execute("SET ROLE wamn_system")
        .await
        .expect("assume control database owner");
    for bootstrap in CONTROL_BOOTSTRAP_SQL {
        target
            .batch_execute(bootstrap)
            .await
            .expect("apply control bootstrap SQL");
    }
    target
        .batch_execute(
            "SET app.tenant='tenant-a'; \
             INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ('tenant-a','cat-a',1,'dev','0.1','draft'); \
             INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             VALUES ('tenant-a','sha256:'||encode(sha256(convert_to('own','UTF8')),'hex'), \
                     '0.1',convert_to('own','UTF8'),3); \
             SET app.tenant='tenant-b'; \
             INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             VALUES ('tenant-b','sha256:'||encode(sha256(convert_to('foreign','UTF8')),'hex'), \
                     '0.1',convert_to('foreign','UTF8'),7); \
             RESET app.tenant; RESET ROLE;",
        )
        .await
        .expect("seed two control-plane tenants");

    let collision_policy = artifact_reader_policy_name(TENANT, DATABASE);
    let public_connect_before = target
        .query(sql::artifact_reader_public_connect_databases_sql(), &[])
        .await
        .expect("read pre-collision PUBLIC CONNECT list")
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    let public_schema_usage_before: bool = target
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_namespace n CROSS JOIN LATERAL \
               aclexplode(n.nspacl) acl WHERE n.nspname='public' \
               AND acl.grantee=0 AND acl.privilege_type='USAGE')",
            &[],
        )
        .await
        .expect("read pre-collision PUBLIC schema floor")
        .get(0);
    assert!(public_schema_usage_before);
    target
        .batch_execute(&format!(
            "CREATE ROLE \"{COLLISION_ROLE}\" NOLOGIN NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE POLICY \"{collision_policy}\" ON catalog.execution_bundles \
               AS RESTRICTIVE FOR SELECT TO \"{COLLISION_ROLE}\" USING (true)"
        ))
        .await
        .expect("install deterministic unrelated policy-name collision");
    let collision = provision_project_env::run(action_args(
        &target_url,
        Some(CredentialGeneration::A),
        None,
        false,
        Some(&secret),
    ))
    .await
    .expect_err("unrelated deterministic policy-name collision admitted initial A");
    let collision_detail = format!("{collision:#}");
    assert!(
        collision_detail.contains("policy set is missing, extra, or drifted"),
        "unexpected policy-collision refusal: {collision_detail}"
    );
    assert!(
        target
            .query_opt(
                sql::artifact_reader_tenant_role_state_sql(),
                &[&stable_role],
            )
            .await
            .expect("probe refused stable role")
            .is_none()
    );
    assert!(
        target
            .query_opt(sql::artifact_reader_generation_state_sql(), &[&role_a],)
            .await
            .expect("probe refused initial A role")
            .is_none()
    );
    assert!(!secret.exists());
    let public_connect_after = target
        .query(sql::artifact_reader_public_connect_databases_sql(), &[])
        .await
        .expect("read post-collision PUBLIC CONNECT list")
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    assert_eq!(public_connect_after, public_connect_before);
    let public_schema_usage_after: bool = target
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_namespace n CROSS JOIN LATERAL \
               aclexplode(n.nspacl) acl WHERE n.nspname='public' \
               AND acl.grantee=0 AND acl.privilege_type='USAGE')",
            &[],
        )
        .await
        .expect("read post-collision PUBLIC schema floor")
        .get(0);
    assert_eq!(public_schema_usage_after, public_schema_usage_before);
    target
        .batch_execute(&format!(
            "DROP POLICY \"{collision_policy}\" ON catalog.execution_bundles; \
             DROP ROLE \"{COLLISION_ROLE}\""
        ))
        .await
        .expect("remove deterministic unrelated policy-name collision");

    provision_project_env::run(action_args(
        &target_url,
        Some(CredentialGeneration::A),
        None,
        false,
        Some(&secret),
    ))
    .await
    .expect("prepare initial artifact-reader A");
    let secret_a = std::fs::read(&secret).expect("read A Secret bytes");
    let (url_a, expires_a) = credential_document(&secret, &target_url, CredentialGeneration::A);
    let (client_a, connection_a) = connect_reader(&url_a, ARTIFACT_READER_APPLICATION_NAME).await;
    assert_reader_authority(&client_a, &stable_role).await;
    assert_stable_role(&target, &stable_role, &[&role_a]).await;
    let reader_public_schema_usage: bool = target
        .query_one(
            "SELECT has_schema_privilege($1, 'public', 'USAGE')",
            &[&role_a],
        )
        .await
        .expect("probe reader PUBLIC schema authority")
        .get(0);
    assert!(!reader_public_schema_usage);
    assert_generation_role(
        &target,
        &role_a,
        &stable_role,
        CredentialGeneration::A,
        Some(&expires_a),
    )
    .await;
    let unrelated_connect: bool = target
        .query_one(
            "SELECT has_database_privilege($1, 'postgres', 'CONNECT')",
            &[&role_a],
        )
        .await
        .expect("probe unrelated database CONNECT")
        .get(0);
    assert!(!unrelated_connect);

    target
        .batch_execute("ALTER TABLE catalog.execution_bundles DISABLE ROW LEVEL SECURITY")
        .await
        .expect("disable execution-bundle RLS");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "disabled RLS admitted B",
        "must keep enabled and forced row security",
    )
    .await;
    target
        .batch_execute("ALTER TABLE catalog.execution_bundles ENABLE ROW LEVEL SECURITY")
        .await
        .expect("restore execution-bundle RLS");

    target
        .batch_execute("ALTER TABLE catalog.execution_bundles NO FORCE ROW LEVEL SECURITY")
        .await
        .expect("unforce execution-bundle RLS");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "unforced RLS admitted B",
        "must keep enabled and forced row security",
    )
    .await;
    target
        .batch_execute("ALTER TABLE catalog.execution_bundles FORCE ROW LEVEL SECURITY")
        .await
        .expect("restore forced execution-bundle RLS");

    target
        .batch_execute(
            "CREATE POLICY artifact_reader_extra_public ON catalog.execution_bundles \
             FOR SELECT TO PUBLIC USING (true)",
        )
        .await
        .expect("inject extra PUBLIC policy");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "extra PUBLIC policy admitted B",
        "policy set is missing, extra, or drifted",
    )
    .await;
    target
        .batch_execute("DROP POLICY artifact_reader_extra_public ON catalog.execution_bundles")
        .await
        .expect("remove extra PUBLIC policy");

    target
        .batch_execute(&format!(
            "CREATE POLICY artifact_reader_extra_generation ON catalog.execution_bundles \
             FOR SELECT TO \"{role_a}\" USING (true)"
        ))
        .await
        .expect("inject generation-targeted policy");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "generation-targeted policy admitted B",
        "policy set is missing, extra, or drifted",
    )
    .await;
    target
        .batch_execute("DROP POLICY artifact_reader_extra_generation ON catalog.execution_bundles")
        .await
        .expect("remove generation-targeted policy");

    target
        .batch_execute("GRANT USAGE ON SCHEMA public TO PUBLIC")
        .await
        .expect("grant PUBLIC schema use");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC schema use admitted B",
        "PUBLIC carries control application authority",
    )
    .await;
    target
        .batch_execute("REVOKE USAGE ON SCHEMA public FROM PUBLIC")
        .await
        .expect("remove PUBLIC schema use");

    target
        .batch_execute("GRANT SELECT ON catalog.execution_bundles TO PUBLIC")
        .await
        .expect("grant PUBLIC relation read");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC relation read admitted B",
        "PUBLIC carries control application authority",
    )
    .await;
    target
        .batch_execute("REVOKE SELECT ON catalog.execution_bundles FROM PUBLIC")
        .await
        .expect("remove PUBLIC relation read");

    target
        .batch_execute("GRANT INSERT ON catalog.execution_bundles TO PUBLIC")
        .await
        .expect("grant PUBLIC relation write");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC relation write admitted B",
        "PUBLIC carries control application authority",
    )
    .await;
    target
        .batch_execute("REVOKE INSERT ON catalog.execution_bundles FROM PUBLIC")
        .await
        .expect("remove PUBLIC relation write");

    target
        .batch_execute("GRANT CREATE ON SCHEMA catalog TO PUBLIC")
        .await
        .expect("grant PUBLIC schema authority");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC schema authority admitted B",
        "PUBLIC carries control application authority",
    )
    .await;
    target
        .batch_execute("REVOKE CREATE ON SCHEMA catalog FROM PUBLIC")
        .await
        .expect("remove PUBLIC schema authority");

    target
        .batch_execute("GRANT EXECUTE ON FUNCTION catalog.reject_immutable_row_change() TO PUBLIC")
        .await
        .expect("grant PUBLIC routine authority");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC routine authority admitted B",
        "PUBLIC carries control application authority",
    )
    .await;
    target
        .batch_execute(
            "REVOKE EXECUTE ON FUNCTION catalog.reject_immutable_row_change() FROM PUBLIC",
        )
        .await
        .expect("remove PUBLIC routine authority");

    target
        .batch_execute(
            "SELECT lo_create(987654321); \
             GRANT SELECT ON LARGE OBJECT 987654321 TO PUBLIC",
        )
        .await
        .expect("grant PUBLIC large-object authority");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC large-object authority admitted B",
        "PUBLIC carries control application authority",
    )
    .await;
    target
        .batch_execute(
            "REVOKE SELECT ON LARGE OBJECT 987654321 FROM PUBLIC; \
             SELECT lo_unlink(987654321)",
        )
        .await
        .expect("remove PUBLIC large-object authority");

    target
        .batch_execute(
            "CREATE FOREIGN DATA WRAPPER artifact_reader_probe_fdw NO HANDLER; \
             GRANT USAGE ON FOREIGN DATA WRAPPER artifact_reader_probe_fdw TO PUBLIC",
        )
        .await
        .expect("grant PUBLIC foreign-data-wrapper authority");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC foreign-data-wrapper authority admitted B",
        "PUBLIC carries control application authority",
    )
    .await;
    target
        .batch_execute(
            "REVOKE USAGE ON FOREIGN DATA WRAPPER artifact_reader_probe_fdw FROM PUBLIC; \
             CREATE SERVER artifact_reader_probe_server \
               FOREIGN DATA WRAPPER artifact_reader_probe_fdw; \
             GRANT USAGE ON FOREIGN SERVER artifact_reader_probe_server TO PUBLIC",
        )
        .await
        .expect("grant PUBLIC foreign-server authority");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC foreign-server authority admitted B",
        "PUBLIC carries control application authority",
    )
    .await;
    target
        .batch_execute(
            "REVOKE USAGE ON FOREIGN SERVER artifact_reader_probe_server FROM PUBLIC; \
             DROP SERVER artifact_reader_probe_server; \
             DROP FOREIGN DATA WRAPPER artifact_reader_probe_fdw",
        )
        .await
        .expect("remove PUBLIC foreign authority");

    target
        .batch_execute("GRANT CREATE ON TABLESPACE pg_default TO PUBLIC")
        .await
        .expect("grant PUBLIC tablespace authority");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC tablespace authority admitted B",
        "PUBLIC carries control application authority",
    )
    .await;
    target
        .batch_execute("REVOKE CREATE ON TABLESPACE pg_default FROM PUBLIC")
        .await
        .expect("remove PUBLIC tablespace authority");

    target
        .batch_execute("GRANT SET ON PARAMETER application_name TO PUBLIC")
        .await
        .expect("grant PUBLIC parameter authority");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC parameter authority admitted B",
        "PUBLIC carries control application authority",
    )
    .await;
    target
        .batch_execute("REVOKE SET ON PARAMETER application_name FROM PUBLIC")
        .await
        .expect("remove PUBLIC parameter authority");

    target
        .batch_execute(
            "ALTER DEFAULT PRIVILEGES FOR ROLE wamn_system IN SCHEMA catalog \
             GRANT SELECT ON TABLES TO PUBLIC",
        )
        .await
        .expect("grant PUBLIC default relation authority");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC default ACL admitted B",
        "PUBLIC carries control application authority",
    )
    .await;
    target
        .batch_execute(
            "ALTER DEFAULT PRIVILEGES FOR ROLE wamn_system IN SCHEMA catalog \
             REVOKE SELECT ON TABLES FROM PUBLIC",
        )
        .await
        .expect("remove PUBLIC default relation authority");

    target
        .batch_execute(&format!(
            "GRANT SELECT (byte_length) ON catalog.execution_bundles \
             TO \"{stable_role}\" WITH GRANT OPTION"
        ))
        .await
        .expect("grant stable role a grant option");
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "stable grant option admitted B",
        "may grant SELECT",
    )
    .await;
    target
        .batch_execute(&format!(
            "REVOKE GRANT OPTION FOR SELECT (byte_length) \
             ON catalog.execution_bundles FROM \"{stable_role}\""
        ))
        .await
        .expect("remove stable role grant option");

    target
        .batch_execute("GRANT CONNECT ON DATABASE template1 TO PUBLIC")
        .await
        .expect("grant PUBLIC CONNECT on template1");
    let effective_template_connect: bool = target
        .query_one(
            "SELECT has_database_privilege($1, 'template1', 'CONNECT')",
            &[&role_a],
        )
        .await
        .expect("probe inherited template1 CONNECT")
        .get(0);
    assert!(effective_template_connect);
    assert_prepare_b_refused(
        &target_url,
        &target,
        &secret,
        &secret_a,
        &role_b,
        "PUBLIC template1 CONNECT admitted B",
        "attributes or authority drifted",
    )
    .await;
    target
        .batch_execute("REVOKE CONNECT ON DATABASE template1 FROM PUBLIC")
        .await
        .expect("remove PUBLIC CONNECT on template1");
    let effective_template_connect: bool = target
        .query_one(
            "SELECT has_database_privilege($1, 'template1', 'CONNECT')",
            &[&role_a],
        )
        .await
        .expect("prove template1 CONNECT removal")
        .get(0);
    assert!(!effective_template_connect);
    assert_stable_role(&target, &stable_role, &[&role_a]).await;
    assert_generation_role(
        &target,
        &role_a,
        &stable_role,
        CredentialGeneration::A,
        Some(&expires_a),
    )
    .await;

    target
        .batch_execute(&format!(
            "REVOKE SELECT (byte_length) ON catalog.execution_bundles FROM \"{stable_role}\""
        ))
        .await
        .expect("inject stable ACL drift");
    let drift = provision_project_env::run(action_args(
        &target_url,
        Some(CredentialGeneration::B),
        None,
        false,
        Some(&secret),
    ))
    .await
    .expect_err("drifted stable ACL admitted B");
    assert!(drift.to_string().contains("unexpected direct ACLs"));
    assert_eq!(std::fs::read(&secret).unwrap(), secret_a);
    assert!(
        target
            .query_opt(sql::artifact_reader_generation_state_sql(), &[&role_b])
            .await
            .expect("probe refused B")
            .is_none()
    );
    target
        .batch_execute(&format!(
            "GRANT SELECT (byte_length) ON catalog.execution_bundles TO \"{stable_role}\""
        ))
        .await
        .expect("restore exact stable ACL");
    assert_stable_role(&target, &stable_role, &[&role_a]).await;

    std::fs::write(&blocked_parent, b"not-a-directory")
        .expect("create deterministic Secret output obstruction");
    let failed_publish = provision_project_env::run(action_args(
        &target_url,
        Some(CredentialGeneration::B),
        None,
        false,
        Some(&blocked_secret),
    ))
    .await
    .expect_err("failed Secret publication retained B authority");
    let failed_publish_diagnostic = format!("{failed_publish:#}");
    assert!(
        failed_publish_diagnostic.contains("credential output"),
        "unexpected failed-publication diagnostic: {failed_publish_diagnostic}"
    );
    assert_eq!(std::fs::read(&secret).unwrap(), secret_a);
    assert_generation_role(
        &target,
        &role_b,
        &stable_role,
        CredentialGeneration::B,
        None,
    )
    .await;
    assert_stable_role(&target, &stable_role, &[&role_a]).await;
    std::fs::remove_file(&blocked_parent).expect("remove Secret output obstruction");

    provision_project_env::run(action_args(
        &target_url,
        Some(CredentialGeneration::B),
        None,
        false,
        Some(&secret),
    ))
    .await
    .expect("prepare overlapping artifact-reader B");
    let secret_b = std::fs::read(&secret).expect("read B Secret bytes");
    let (url_b, expires_b) = credential_document(&secret, &target_url, CredentialGeneration::B);
    assert_ne!(secret_b, secret_a);
    assert_stable_role(&target, &stable_role, &[&role_a, &role_b]).await;
    assert_generation_role(
        &target,
        &role_b,
        &stable_role,
        CredentialGeneration::B,
        Some(&expires_b),
    )
    .await;

    let mut secret_with_data: serde_json::Value =
        serde_json::from_slice(&secret_b).expect("parse B Secret for closed-shape red");
    secret_with_data["data"] = serde_json::json!({"extra": "bXV0YW50"});
    std::fs::write(
        &secret,
        serde_json::to_vec_pretty(&secret_with_data).expect("serialize closed-shape red"),
    )
    .expect("write Secret with forbidden data field");
    let extra_data = provision_project_env::run(action_args(
        &target_url,
        None,
        Some(CredentialGeneration::A),
        false,
        Some(&secret),
    ))
    .await
    .expect_err("normal retirement accepted a Secret with a second data surface");
    assert!(
        extra_data
            .to_string()
            .contains("must not contain a data field")
    );
    std::fs::write(&secret, &secret_b).expect("restore exact published B Secret");

    let no_replacement = provision_project_env::run(action_args(
        &target_url,
        None,
        Some(CredentialGeneration::A),
        false,
        Some(&secret),
    ))
    .await
    .expect_err("retirement accepted B without exact executor use");
    assert!(
        no_replacement
            .to_string()
            .contains("no independent executor session")
    );

    let (client_b, connection_b) = connect_reader(&url_b, ARTIFACT_READER_APPLICATION_NAME).await;
    assert_reader_authority(&client_b, &stable_role).await;
    let published_slot = provision_project_env::run(action_args(
        &target_url,
        None,
        Some(CredentialGeneration::B),
        false,
        Some(&secret),
    ))
    .await
    .expect_err("normal retirement revoked the generation named by the published Secret");
    assert!(
        published_slot
            .to_string()
            .contains("published artifact-reader Secret")
    );
    assert!(client_b.simple_query("SELECT 1").await.is_ok());
    target
        .batch_execute(&format!(
            "ALTER ROLE \"{role_a}\" VALID UNTIL '2000-01-01 00:00:00+00'"
        ))
        .await
        .expect("expire old A without revoking its existing session");
    let old_expired_and_replacement_unexpired = target
        .query_one(
            "SELECT old.rolvaliduntil < now(), replacement.rolvaliduntil > now() \
             FROM pg_authid old CROSS JOIN pg_authid replacement \
             WHERE old.rolname=$1 AND replacement.rolname=$2",
            &[&role_a, &role_b],
        )
        .await
        .expect("prove old A expired and replacement B unexpired");
    assert!(old_expired_and_replacement_unexpired.get::<_, bool>(0));
    assert!(old_expired_and_replacement_unexpired.get::<_, bool>(1));
    provision_project_env::run(action_args(
        &target_url,
        None,
        Some(CredentialGeneration::A),
        false,
        Some(&secret),
    ))
    .await
    .expect("retire A after exact named B replacement use");
    assert!(client_a.simple_query("SELECT 1").await.is_err());
    let _ = connection_a.await;
    client_b
        .simple_query("SELECT 1")
        .await
        .expect("unexpired named-session B survives ordinary A retirement");
    let replacement_sessions: i64 = target
        .query_one(
            sql::artifact_reader_replacement_use_sql(),
            &[&role_b, &DATABASE, &ARTIFACT_READER_APPLICATION_NAME],
        )
        .await
        .expect("count exact surviving B executor session")
        .get(0);
    assert_eq!(replacement_sessions, 1);
    assert_generation_role(
        &target,
        &role_a,
        &stable_role,
        CredentialGeneration::A,
        None,
    )
    .await;
    assert_generation_role(
        &target,
        &role_b,
        &stable_role,
        CredentialGeneration::B,
        Some(&expires_b),
    )
    .await;
    assert_stable_role(&target, &stable_role, &[&role_b]).await;

    provision_project_env::run(action_args(&target_url, None, None, true, Some(&secret)))
        .await
        .expect("emergency revoke both artifact-reader slots");
    assert!(!secret.exists());
    assert!(client_b.simple_query("SELECT 1").await.is_err());
    let _ = connection_b.await;
    assert_generation_role(
        &target,
        &role_a,
        &stable_role,
        CredentialGeneration::A,
        None,
    )
    .await;
    assert_generation_role(
        &target,
        &role_b,
        &stable_role,
        CredentialGeneration::B,
        None,
    )
    .await;
    assert_stable_role(&target, &stable_role, &[]).await;

    drop(target);
    catalog
        .batch_execute(&format!("DROP DATABASE \"{DATABASE}\" WITH (FORCE)"))
        .await
        .expect("drop disposable artifact-reader database");
    catalog
        .batch_execute(&format!(
            "DROP ROLE \"{role_a}\"; DROP ROLE \"{role_b}\"; DROP ROLE \"{stable_role}\"; \
             DROP ROLE IF EXISTS \"{COLLISION_ROLE}\";"
        ))
        .await
        .expect("clean disposable artifact-reader roles");
    let _ = std::fs::remove_file(secret);
    let _ = std::fs::remove_file(blocked_parent);
}
