//! PG18 lifecycle proof for the management-admitter A/B generation pair.
//!
//! `wamn-0h0g.12.176` completes `wamn-0h0g.12.118`'s deferral — "no bespoke
//! prepare, retire, Secret, or A/B implementation", closed at the time for want
//! of "a ctl lifecycle or call site". `wamn-0h0g.8.5.3` landed the call site, so
//! this proves the ctl stamp drives the generic `wamn-0h0g.13.59` lifecycle
//! against a real cluster: nothing here reimplements prepare, retire or abort.
//!
//! Run only against a disposable cluster: the test creates one database and
//! cluster-global roles, revokes PUBLIC CONNECT on every non-template database,
//! and revokes PUBLIC TEMPORARY on the exact project database.
//!
//! `WAMN_MANAGEMENT_ADMITTER_PG18_URL=postgres://.../postgres cargo test -p wamn-ctl \
//!   --test management_admitter_generation_live -- --ignored --nocapture`

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tokio_postgres::{Client, NoTls};
use url::Url;

use wamn_control_provision::tenant_key::authority_derivations_bootstrap_sql;
use wamn_control_provision::{
    CredentialGeneration, MANAGEMENT_ADMITTER_ROLE, PLATFORM_GROUP_ROLE, WorkloadRoleFamily,
    management_admitter_generation_role, management_admitter_secret_name,
    parse_management_admission_url, project_env_database_name,
    render_management_admitter_secret_manifest, sql,
};
use wamn_control_registry::Triple;
use wamn_ctl::provision_project_env::{
    self, ProvisionProjectEnvArgs, WorkloadActionVerb, WorkloadGenerationAction,
    WorkloadGenerationArgs,
};

const ORG: &str = "pg18admit";
const PROJECT: &str = "receiving";
const ENVIRONMENT: &str = "dev";
/// Required by the shared workload-action identity contract and deliberately
/// NOT an input to this family's scope digest — the derived-pair assertions in
/// the test body are what keep it that way.
const TENANT: &str = "tenant-live";
const INSTANCE: &str = "k3m9x2p7";
const RUN_SCHEMA: &str = "wamn_run";

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
        "wamn-mgmt-admitter-pg18-{}-{}.json",
        std::process::id(),
        generation.as_str()
    ))
}

fn action_args(
    target_admin_url: &str,
    prepare: Option<(CredentialGeneration, &Path)>,
    retire: Option<CredentialGeneration>,
    abort: Option<CredentialGeneration>,
) -> ProvisionProjectEnvArgs {
    let mut system_url = Url::parse(target_admin_url).expect("parse target admin URL");
    system_url.set_path("/postgres");
    system_url.set_query(None);
    system_url.set_fragment(None);
    ProvisionProjectEnvArgs {
        org: Some(ORG.to_string()),
        project: Some(PROJECT.to_string()),
        env: Some(ENVIRONMENT.to_string()),
        tenant: Some(TENANT.to_string()),
        system_database_url: Some(system_url.into()),
        cluster: None,
        connection_limit: None,
        // wamn-0h0g.12.141 exempts every credential-free action mode from both
        // role-batch passwords, and this is one, so `None` is what a real
        // invocation carries.
        app_password: None,
        app_host: None,
        app_port: 5432,
        namespace: "wamn-system".to_string(),
        secret_namespace: None,
        target_admin_database_url: Some(target_admin_url.to_string()),
        emit_database: None,
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: None,
        emit_management_author_pat_secret: None,
        emit_route_caller_pat_secret: None,
        revoke_pat_prefix: None,
        // wamn-0h0g.22.16: one derived action, not four families of fields.
        workload: WorkloadGenerationArgs {
            action: prepare
                .map(|(generation, _)| (WorkloadActionVerb::Prepare, generation))
                .or_else(|| retire.map(|generation| (WorkloadActionVerb::Retire, generation)))
                .or_else(|| abort.map(|generation| (WorkloadActionVerb::Abort, generation)))
                .map(|(verb, generation)| WorkloadGenerationAction {
                    family: WorkloadRoleFamily::ManagementAdmitter,
                    verb,
                    generation,
                }),
            secret: prepare
                .map(|(_, path)| (WorkloadRoleFamily::ManagementAdmitter, path.to_path_buf())),
        },
    }
}

/// The emitted Secret's URL, proven in-scope by the pure consumer gate rather
/// than by re-deriving the role name here.
fn secret_url(path: &Path, database: &str) -> String {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("read emitted Secret"))
            .expect("parse emitted Secret");
    // The mint is named by the same helper `deploy/platform/scenario-worker.yaml`
    // references, so the reference cannot drift from what ctl writes.
    assert_eq!(
        manifest["metadata"]["name"].as_str().expect("Secret name"),
        management_admitter_secret_name(ORG, PROJECT, ENVIRONMENT)
    );
    let data = manifest["stringData"].as_object().expect("stringData");
    assert_eq!(data.len(), 1, "the Secret carries exactly one key");
    let url = data["url"].as_str().expect("url stringData").to_string();
    let connection = parse_management_admission_url(&url, ORG, PROJECT, ENVIRONMENT)
        .expect("the emitted URL passes the consumer's own fail-closed gate");
    assert_eq!(connection.database(), database);
    url
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
    finite_valid_until: bool,
    memberships: &[&str],
    member_roles: &[&str],
    connect_databases: &[&str],
) {
    let row = admin
        .query_one(sql::workload_generation_state_sql(), &[&role])
        .await
        .expect("read exact role state");
    assert_eq!(row.get::<_, bool>("rolcanlogin"), login, "{role} LOGIN");
    assert!(!row.get::<_, bool>("rolsuper"));
    assert_eq!(row.get::<_, bool>("rolinherit"), inherit, "{role} INHERIT");
    assert!(!row.get::<_, bool>("rolcreaterole"));
    assert!(!row.get::<_, bool>("rolcreatedb"));
    assert!(!row.get::<_, bool>("rolreplication"));
    assert!(!row.get::<_, bool>("rolbypassrls"));
    assert_eq!(
        row.get::<_, bool>("password_set"),
        password_set,
        "{role} password"
    );
    assert_eq!(
        row.get::<_, bool>("valid_until_finite"),
        finite_valid_until,
        "{role} VALID UNTIL"
    );
    assert_eq!(
        row.get::<_, Vec<String>>("memberships"),
        memberships
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>(),
        "{role} memberships"
    );
    assert!(row.get::<_, bool>("membership_options_exact"));
    assert_eq!(
        row.get::<_, Vec<String>>("member_roles"),
        member_roles
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>(),
        "{role} member roles"
    );
    assert!(row.get::<_, bool>("member_options_exact"));
    assert!(row.get::<_, bool>("generation_children_exact"));
    assert_eq!(
        row.get::<_, Vec<String>>("connect_databases"),
        connect_databases
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>(),
        "{role} CONNECT"
    );
    assert_eq!(row.get::<_, i64>("owned_objects"), 0);
}

/// Wait for a closed client's backend to actually exit.
///
/// `abort` refuses a generation with any live session, and a dropped client's
/// backend disappears from `pg_stat_activity` slightly after the socket closes.
async fn await_zero_sessions(admin: &Client, role: &str) {
    for _ in 0..100 {
        let sessions: i64 = admin
            .query_one(sql::workload_generation_state_sql(), &[&role])
            .await
            .expect("read role sessions")
            .get("sessions");
        if sessions == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("{role} still has a live session");
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

/// The observable converged state of the whole family, as one comparable value.
///
/// R55: a converge path is proven by a SECOND run being a no-op, not by its exit
/// status. The password and `VALID UNTIL` are re-minted by design, so they are
/// deliberately outside this snapshot — everything else must be identical.
async fn family_state(admin: &Client, roles: &[&str]) -> Vec<(String, Vec<String>, Vec<String>)> {
    let mut snapshot = Vec::new();
    for role in roles {
        let row = admin
            .query_opt(sql::workload_generation_state_sql(), &[role])
            .await
            .expect("read family role state");
        let members = match &row {
            None => vec!["<absent>".to_string()],
            Some(row) => {
                let mut members = row.get::<_, Vec<String>>("memberships");
                members.extend(row.get::<_, Vec<String>>("member_roles"));
                members.extend(row.get::<_, Vec<String>>("connect_databases"));
                members.push(format!("login={}", row.get::<_, bool>("rolcanlogin")));
                members
            }
        };
        let acl: Vec<String> = direct_acl_set(admin, role).await.into_iter().collect();
        snapshot.push(((*role).to_string(), members, acl));
    }
    snapshot
}

/// The exact fixture the grant batch addresses, derived from the SAME frozen
/// constants the ACL verifier expects, so the fixture cannot drift away from the
/// grant set and quietly widen what "exact" means.
fn run_plane_fixture() -> String {
    let mut run_columns: BTreeSet<&str> = BTreeSet::new();
    run_columns.extend(sql::MANAGEMENT_ADMITTER_RUN_SELECT_COLUMNS);
    run_columns.extend(sql::MANAGEMENT_ADMITTER_RUN_INSERT_COLUMNS);
    let mut queue_columns: BTreeSet<&str> = BTreeSet::new();
    queue_columns.extend(sql::MANAGEMENT_ADMITTER_QUEUE_SELECT_COLUMNS);
    queue_columns.extend(sql::MANAGEMENT_ADMITTER_QUEUE_INSERT_COLUMNS);
    let mut wiring_columns: BTreeSet<&str> = BTreeSet::from(["created_at"]);
    wiring_columns.extend(sql::MANAGEMENT_ADMITTER_WIRING_INSERT_COLUMNS);
    let column_ddl = |columns: &BTreeSet<&str>| {
        columns
            .iter()
            .map(|column| format!("\"{column}\" text"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut ddl = format!(
        "CREATE SCHEMA catalog; CREATE SCHEMA {RUN_SCHEMA}; CREATE SCHEMA unrelated; \
         CREATE TABLE {RUN_SCHEMA}.environment_policies (id bigint); \
         CREATE TABLE {RUN_SCHEMA}.runs ({runs}); \
         CREATE TABLE {RUN_SCHEMA}.run_queue ({queue}); \
         CREATE TABLE {RUN_SCHEMA}.untouched (id bigint); \
         CREATE TABLE unrelated.probe (id bigint);",
        runs = column_ddl(&run_columns),
        queue = column_ddl(&queue_columns),
    );
    for relation in sql::MANAGEMENT_ADMITTER_CATALOG_RELATIONS {
        if relation == "wirings" {
            ddl.push_str(&format!(
                " CREATE TABLE catalog.\"{relation}\" ({columns});",
                columns = column_ddl(&wiring_columns),
            ));
        } else {
            ddl.push_str(&format!(
                " CREATE TABLE catalog.\"{relation}\" (id bigint);"
            ));
        }
    }
    ddl
}

fn expected_stable_acl() -> BTreeSet<String> {
    let mut expected = BTreeSet::from([
        "schema:catalog:catalog:USAGE".to_string(),
        format!("schema:{RUN_SCHEMA}:{RUN_SCHEMA}:USAGE"),
        format!("relation:{RUN_SCHEMA}:environment_policies:SELECT"),
        // `runs` carries `runs_tkey`, so the INSERT grants above are dead
        // without this EXECUTE. Spelled as a literal, not read back from the
        // builder that emits the grant: a row derived from the same constant
        // that produced it would assert nothing about what the server did.
        "routine:wamn_authority:tenant_key:EXECUTE".to_string(),
    ]);
    for relation in sql::MANAGEMENT_ADMITTER_CATALOG_RELATIONS {
        expected.insert(format!("relation:catalog:{relation}:SELECT"));
    }
    for column in sql::MANAGEMENT_ADMITTER_WIRING_INSERT_COLUMNS {
        expected.insert(format!("column:catalog:wirings.{column}:INSERT"));
    }
    for (relation, privilege, columns) in [
        (
            "runs",
            "SELECT",
            &sql::MANAGEMENT_ADMITTER_RUN_SELECT_COLUMNS[..],
        ),
        (
            "runs",
            "INSERT",
            &sql::MANAGEMENT_ADMITTER_RUN_INSERT_COLUMNS[..],
        ),
        (
            "run_queue",
            "SELECT",
            &sql::MANAGEMENT_ADMITTER_QUEUE_SELECT_COLUMNS[..],
        ),
        (
            "run_queue",
            "INSERT",
            &sql::MANAGEMENT_ADMITTER_QUEUE_INSERT_COLUMNS[..],
        ),
    ] {
        for column in columns {
            expected.insert(format!(
                "column:{RUN_SCHEMA}:{relation}.{column}:{privilege}"
            ));
        }
    }
    expected
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL 18 via WAMN_MANAGEMENT_ADMITTER_PG18_URL"]
async fn management_admitter_generation_lifecycle_converges_and_rotates() {
    let admin_url = std::env::var("WAMN_MANAGEMENT_ADMITTER_PG18_URL")
        .expect("set WAMN_MANAGEMENT_ADMITTER_PG18_URL to a disposable PG18 superuser URL");
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

    let database = project_env_database_name(ORG, PROJECT, ENVIRONMENT, INSTANCE);
    let role_a = management_admitter_generation_role(
        ORG,
        PROJECT,
        ENVIRONMENT,
        &database,
        CredentialGeneration::A,
    );
    let role_b = management_admitter_generation_role(
        ORG,
        PROJECT,
        ENVIRONMENT,
        &database,
        CredentialGeneration::B,
    );
    let pair = [role_a.as_str(), role_b.as_str(), MANAGEMENT_ADMITTER_ROLE];

    // The tenant is identity context, never scope: the derived pair must be the
    // same one the pure crate derives without it.
    assert_ne!(role_a, role_b);
    assert!(!role_a.contains(TENANT));

    // HERMETIC PREAMBLE. The stable ACL role is DROPPED, not left healthy: a
    // surviving one satisfies `ensure_workload_acl_role_sql`'s IF NOT EXISTS
    // guard and would mask a mutated builder.
    catalog
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE;",
        )
        .await
        .expect("reset registry schemas");
    catalog
        .batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .await
        .expect("install registry schema");
    catalog
        .batch_execute(&format!(
            "ALTER TABLE registry.project_envs OWNER TO wamn_system; \
             INSERT INTO registry.orgs (id, placement_kind, pool_cluster) \
             VALUES ('{ORG}', 'pooled', 'pool') \
             ON CONFLICT (id) DO NOTHING; \
             INSERT INTO registry.env_policies \
               (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image, \
                backup_cadence, wal_retention, hibernation) \
             VALUES ('{ORG}', '{ENVIRONMENT}', '{{\"kind\":\"own\"}}', 1, 1, '1Gi', '250m', \
                     '256Mi', 'postgres:18', '', '', 'off') \
             ON CONFLICT (org, name) DO NOTHING; \
             INSERT INTO registry.projects (org, id) VALUES ('{ORG}', '{PROJECT}') \
             ON CONFLICT (org, id) DO NOTHING; \
             INSERT INTO registry.project_envs \
               (org, project, env, secret_name, instance_suffix) \
             VALUES ('{ORG}', '{PROJECT}', '{ENVIRONMENT}', \
                     'wamn-db-{ORG}--{PROJECT}--{ENVIRONMENT}', '{INSTANCE}') \
             ON CONFLICT (org, project, env) DO UPDATE SET instance_suffix = EXCLUDED.instance_suffix;"
        ))
        .await
        .expect("install stored project-env instance");
    catalog
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS \"{database}\" WITH (FORCE)"
        ))
        .await
        .expect("drop prior lifecycle database");
    catalog
        .batch_execute(&format!(
            "DROP ROLE IF EXISTS \"{role_a}\"; DROP ROLE IF EXISTS \"{role_b}\"; \
             DROP ROLE IF EXISTS \"{MANAGEMENT_ADMITTER_ROLE}\"; \
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
    catalog
        .batch_execute(&sql::grant_connect_on_database_sql(&database))
        .await
        .expect("revoke target PUBLIC TEMPORARY and grant app CONNECT");

    let target_url = database_url(&admin_url, &database);
    let target = connect(&target_url).await;
    target
        .batch_execute(&run_plane_fixture())
        .await
        .expect("create management-admission fixtures");
    // The prepare batch grants EXECUTE on `wamn_authority.tenant_key`, so the
    // schema has to exist before it runs. Applied from the BUILDER rather than
    // a hand-written twin: `deploy_sql_authority` pins every production DDL
    // file to this exact function's output byte for byte, so a fixture that
    // calls it cannot drift away from what a real database is installed with.
    target
        .batch_execute(&authority_derivations_bootstrap_sql())
        .await
        .expect("install the authority derivations");

    let secret_a = secret_path(CredentialGeneration::A);
    let secret_b = secret_path(CredentialGeneration::B);

    // ---- prepare A -------------------------------------------------------
    provision_project_env::run(action_args(
        &target_url,
        Some((CredentialGeneration::A, &secret_a)),
        None,
        None,
    ))
    .await
    .expect("prepare initial A");
    let url_a = secret_url(&secret_a, &database);
    assert!(
        target
            .query(sql::public_connect_databases_sql(), &[])
            .await
            .expect("ctl converged PUBLIC CONNECT floor")
            .is_empty()
    );
    assert_role(
        &target,
        MANAGEMENT_ADMITTER_ROLE,
        false,
        false,
        false,
        false,
        // The management admitter is platform grain, so its stable ACL role
        // carries the one parent edge `expected_acl_parents` allows — ctl's own
        // prepare-time verifier already requires it.
        &[PLATFORM_GROUP_ROLE],
        &[&role_a],
        &[],
    )
    .await;
    assert_role(
        &target,
        &role_a,
        true,
        true,
        true,
        true,
        &[MANAGEMENT_ADMITTER_ROLE],
        &[],
        &[&database],
    )
    .await;
    assert_eq!(
        direct_acl_set(&target, &role_a).await,
        BTreeSet::from([format!("database:{database}:{database}:CONNECT")]),
        "a generation carries CONNECT and nothing else"
    );
    assert_eq!(
        direct_acl_set(&target, MANAGEMENT_ADMITTER_ROLE).await,
        expected_stable_acl(),
        "the stable ACL role carries exactly the management-admission surface"
    );
    // The emitted URL authenticates; the session is dropped before the converge
    // replay, because prepare over an active generation requires zero sessions.
    {
        let (client, connection) = tokio_postgres::connect(&url_a, NoTls)
            .await
            .expect("authenticate A from the emitted Secret");
        let task = tokio::spawn(connection);
        let who: String = client
            .query_one("SELECT current_user::text", &[])
            .await
            .expect("read authenticated identity")
            .get(0);
        assert_eq!(who, role_a);
        drop(client);
        let _ = task.await;
    }
    let after_first = family_state(&target, &pair).await;

    // ---- R55: the SECOND prepare of A is a no-op on the converged state ----
    provision_project_env::run(action_args(
        &target_url,
        Some((CredentialGeneration::A, &secret_a)),
        None,
        None,
    ))
    .await
    .expect("re-preparing the active zero-session A converges instead of failing");
    let replayed_url = secret_url(&secret_a, &database);
    assert_eq!(
        family_state(&target, &pair).await,
        after_first,
        "a second prepare changed the converged family state"
    );
    assert_ne!(
        replayed_url, url_a,
        "a re-prepare re-mints the credential rather than republishing the old one"
    );
    {
        let (client, connection) = tokio_postgres::connect(&replayed_url, NoTls)
            .await
            .expect("authenticate A from the replayed Secret");
        let task = tokio::spawn(connection);
        drop(client);
        let _ = task.await;
    }

    // ---- prepare B, then retire A: the A/B rotation ------------------------
    provision_project_env::run(action_args(
        &target_url,
        Some((CredentialGeneration::B, &secret_b)),
        None,
        None,
    ))
    .await
    .expect("prepare B alongside the active A");
    let url_b = secret_url(&secret_b, &database);
    assert_ne!(url_b, replayed_url);
    assert_role(
        &target,
        MANAGEMENT_ADMITTER_ROLE,
        false,
        false,
        false,
        false,
        &[PLATFORM_GROUP_ROLE],
        &[&role_a, &role_b],
        &[],
    )
    .await;

    // Retirement is USE-PROVEN: without a live session on the replacement, the
    // old credential is never withdrawn.
    let before_refusal = family_state(&target, &pair).await;
    provision_project_env::run(action_args(
        &target_url,
        None,
        Some(CredentialGeneration::A),
        None,
    ))
    .await
    .expect_err("retiring A before B is serving must refuse");
    assert_eq!(
        family_state(&target, &pair).await,
        before_refusal,
        "a refused retire mutated the family state"
    );

    let (client_b, connection_b) = tokio_postgres::connect(&url_b, NoTls)
        .await
        .expect("authenticate B from the emitted Secret");
    let task_b = tokio::spawn(connection_b);
    let serving: String = client_b
        .query_one("SELECT current_user::text", &[])
        .await
        .expect("read the serving identity")
        .get(0);
    assert_eq!(serving, role_b);

    provision_project_env::run(action_args(
        &target_url,
        None,
        Some(CredentialGeneration::A),
        None,
    ))
    .await
    .expect("retire A once B is proven to be serving");
    assert_role(
        &target,
        MANAGEMENT_ADMITTER_ROLE,
        false,
        false,
        false,
        false,
        &[PLATFORM_GROUP_ROLE],
        &[&role_b],
        &[],
    )
    .await;
    // The retired slot is authentication-free and authority-free, and remains a
    // reusable slot rather than being dropped.
    assert_role(&target, &role_a, false, true, false, true, &[], &[], &[]).await;
    assert!(direct_acl_set(&target, &role_a).await.is_empty());
    assert!(
        tokio_postgres::connect(&replayed_url, NoTls).await.is_err(),
        "the retired generation's URL still authenticates"
    );

    // A SECOND retire of the settled slot changes nothing. Retirement is a
    // one-way transition, so the replay refuses fail-closed rather than
    // converging silently — but it must leave the family exactly as it found it.
    let settled = family_state(&target, &pair).await;
    provision_project_env::run(action_args(
        &target_url,
        None,
        Some(CredentialGeneration::A),
        None,
    ))
    .await
    .expect_err("re-retiring the already-retired A must refuse");
    assert_eq!(
        family_state(&target, &pair).await,
        settled,
        "a replayed retire mutated the family state"
    );

    drop(client_b);
    let _ = task_b.await;
    await_zero_sessions(&target, &role_b).await;

    // ---- abort B: the unpublished-prepare escape hatch ---------------------
    provision_project_env::run(action_args(
        &target_url,
        None,
        None,
        Some(CredentialGeneration::B),
    ))
    .await
    .expect("abort the unpublished B");
    assert_role(&target, &role_b, false, true, false, true, &[], &[], &[]).await;
    assert_role(
        &target,
        MANAGEMENT_ADMITTER_ROLE,
        false,
        false,
        false,
        false,
        &[PLATFORM_GROUP_ROLE],
        &[],
        &[],
    )
    .await;
    assert!(
        tokio_postgres::connect(&url_b, NoTls).await.is_err(),
        "the aborted generation's URL still authenticates"
    );
    // The stable role keeps its surface: aborting a generation is not a
    // teardown of the family.
    assert_eq!(
        direct_acl_set(&target, MANAGEMENT_ADMITTER_ROLE).await,
        expected_stable_acl()
    );

    // The Secret this lifecycle publishes is the renderer's, for the triple the
    // action was invoked with.
    let rendered = render_management_admitter_secret_manifest(
        &Triple::new(ORG, PROJECT, ENVIRONMENT),
        "wamn-system",
        &url_b,
    );
    let emitted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&secret_b).expect("read emitted Secret"))
            .expect("parse emitted Secret");
    assert_eq!(rendered, emitted);

    for path in [&secret_a, &secret_b] {
        let _ = std::fs::remove_file(path);
    }
}
