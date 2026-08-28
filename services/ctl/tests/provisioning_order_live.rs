//! PG18 proof that an operator can run `provision-project-env`'s OWN documented
//! order end to end, and that a prepare which refuses leaves a documented state.
//!
//! `wamn-0h0g.12.179`. Every other live arm for the workload lifecycle builds
//! its cluster by hand and deliberately SIDESTEPS the emitted privilege SQL
//! (`guest_generation_live` says so in a comment). That is exactly why the
//! defect survived: nothing applied [`privilege_sql`] and then asked the verb's
//! next step to accept the result. This file does, with the SAME text
//! production emits — never a transcription.
//!
//! Run only against a disposable cluster: it drops and recreates cluster-global
//! roles, including the stable `wamn_app` ACL role.
//!
//! `WAMN_PROVISIONING_ORDER_PG18_URL=postgres://.../postgres cargo test -p wamn-ctl \
//!   --test provisioning_order_live -- --ignored --nocapture --test-threads=1`

use std::path::{Path, PathBuf};

use tokio_postgres::{Client, NoTls};
use url::Url;

use wamn_control_provision::tenant_key::authority_derivations_bootstrap_sql;
use wamn_control_provision::{
    APP_ROLE, CredentialGeneration, DISPATCH_READER_ROLE, WorkloadRoleFamily, WorkloadRoleScope,
    project_env_database_name, sql, workload_generation_role,
};
use wamn_ctl::provision_project_env::{
    self, ProvisionProjectEnvArgs, WorkloadActionVerb, WorkloadGenerationAction,
    WorkloadGenerationArgs, privilege_sql, role_posture_sql,
};

const ORG: &str = "pg18order";
const PROJECT: &str = "receiving";
const ENVIRONMENT: &str = "dev";
const TENANT: &str = "tenant-order";
const INSTANCE: &str = "k3m9x2p7";
const APP_PASSWORD: &str = "app-secret-order";

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

fn secret_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("wamn-order-pg18-{}-{tag}.json", std::process::id()))
}

fn prepare_args(
    target_admin_url: &str,
    generation: CredentialGeneration,
    path: &Path,
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
        app_host: None,
        app_password: None,
        app_port: 5432,
        namespace: format!("wamn-{ORG}--{PROJECT}--{ENVIRONMENT}--{INSTANCE}"),
        secret_namespace: None,
        target_admin_database_url: Some(target_admin_url.to_string()),
        emit_database: None,
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: None,
        emit_management_author_pat_secret: None,
        emit_route_caller_pat_secret: None,
        revoke_pat_prefix: None,
        workload: WorkloadGenerationArgs {
            action: Some(WorkloadGenerationAction {
                family: WorkloadRoleFamily::App,
                verb: WorkloadActionVerb::Prepare,
                generation,
            }),
            secret: Some((WorkloadRoleFamily::App, path.to_path_buf())),
        },
    }
}

async fn role_state(admin: &Client, role: &str) -> Option<tokio_postgres::Row> {
    admin
        .query_opt(sql::workload_generation_state_sql(), &[&role])
        .await
        .expect("read exact role state")
}

/// THE SERVER'S OWN ANSWER for one role's direct database `CONNECT` ACLs, read
/// through `aclexplode` on `pg_database.datacl` rather than any Rust shape.
async fn connect_databases(admin: &Client, role: &str) -> Vec<String> {
    admin
        .query(
            "SELECT d.datname::text FROM pg_database d \
               CROSS JOIN LATERAL aclexplode(d.datacl) acl \
               JOIN pg_roles r ON r.oid = acl.grantee \
              WHERE r.rolname = $1 AND acl.privilege_type = 'CONNECT' \
              ORDER BY 1",
            &[&role],
        )
        .await
        .expect("read direct database CONNECT ACLs")
        .into_iter()
        .map(|row| row.get(0))
        .collect()
}

/// Rebuild the cluster to the state the runbook's step 0 assumes: the registry
/// rows exist, and NO `wamn_app` / `wamn_dispatch_reader` / generation role
/// survives. The stable ACL role is DROPPED, not left healthy — a surviving one
/// satisfies the ensure-ACL builder's `IF NOT EXISTS` guard and masks a mutated
/// builder.
async fn reset_cluster(catalog: &Client, database: &str, roles: &[&str]) {
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
             VALUES ('{ORG}', 'pooled', 'pool') ON CONFLICT (id) DO NOTHING; \
             INSERT INTO registry.env_policies \
               (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, \
                image, backup_cadence, wal_retention, hibernation) \
             VALUES ('{ORG}', '{ENVIRONMENT}', '{{\"kind\":\"own\"}}', 1, 1, '1Gi', '250m', \
                     '256Mi', 'postgres:18', '', '', 'off') \
             ON CONFLICT (org, name) DO NOTHING; \
             INSERT INTO registry.projects (org, id) VALUES ('{ORG}', '{PROJECT}') \
             ON CONFLICT (org, id) DO NOTHING; \
             INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix) \
             VALUES ('{ORG}', '{PROJECT}', '{ENVIRONMENT}', \
                     'wamn-db-{ORG}--{PROJECT}--{ENVIRONMENT}', '{INSTANCE}') \
             ON CONFLICT (org, project, env) DO UPDATE SET instance_suffix = EXCLUDED.instance_suffix;"
        ))
        .await
        .expect("install stored project-env instance");
    // The database goes FIRST: its ACL rows are what would otherwise refuse the
    // role drops below.
    catalog
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS \"{database}\" WITH (FORCE)"
        ))
        .await
        .expect("drop prior lifecycle database");
    for role in roles {
        catalog
            .batch_execute(&format!(
                "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
                   EXECUTE format('DROP OWNED BY %I CASCADE', '{role}'); \
                   EXECUTE format('DROP ROLE %I', '{role}'); \
                 END IF; END $$;"
            ))
            .await
            .unwrap_or_else(|error| panic!("drop {role}: {error}"));
    }
    // THE CLUSTER FLOOR the verb requires, and the one piece the runbook does
    // not emit: PUBLIC CONNECT off template1 and postgres. A stock container
    // leaves both connectable to PUBLIC.
    catalog
        .batch_execute(sql::revoke_public_connect_floor_sql())
        .await
        .expect("converge the cluster PUBLIC CONNECT floor");
}

/// Apply the runbook's steps 1-3 with the EXACT text the verb emits.
///
/// Step 2 is the CNPG `Database` CR, which this arm stands in for with a plain
/// `CREATE DATABASE` owned by the connecting superuser — the harder case, since
/// step 3's `ALTER DATABASE … OWNER TO` then has an outgoing owner ACL entry to
/// rewrite, which is the ordering hazard `privilege_sql` documents.
async fn apply_documented_order(catalog: &Client, database: &str) {
    catalog
        .batch_execute(&role_posture_sql(APP_PASSWORD))
        .await
        .expect("step 1a: role posture SQL");
    catalog
        .batch_execute(&sql::drain_app_role_sessions_sql())
        .await
        .expect("step 1b: retired shared-login session drain");
    catalog
        .batch_execute(&format!("CREATE DATABASE \"{database}\""))
        .await
        .expect("step 2: the Database CR stand-in");
    catalog
        .batch_execute(&privilege_sql(database))
        .await
        .expect("step 3: privilege SQL");
}

/// THE ARM THE DEFECT BROKE: steps 1-3 as emitted, then the verb's own next
/// step. Nothing here is hand-built; if `privilege_sql` leaves the cluster in a
/// state `--prepare-guest-generation` refuses, this fails.
#[tokio::test]
#[ignore = "requires a disposable PG18 named by WAMN_PROVISIONING_ORDER_PG18_URL"]
async fn the_documented_provisioning_order_completes_end_to_end() {
    let Ok(admin_url) = std::env::var("WAMN_PROVISIONING_ORDER_PG18_URL") else {
        eprintln!("skipping provisioning_order_live (set WAMN_PROVISIONING_ORDER_PG18_URL to run)");
        return;
    };
    let database = project_env_database_name(ORG, PROJECT, ENVIRONMENT, INSTANCE);
    let scope = WorkloadRoleScope::Tenant {
        tenant: TENANT,
        database: &database,
    };
    let role_a = workload_generation_role(WorkloadRoleFamily::App, scope, CredentialGeneration::A)
        .expect("App takes a tenant scope");

    let catalog = connect(&admin_url).await;
    reset_cluster(
        &catalog,
        &database,
        &[&role_a, APP_ROLE, DISPATCH_READER_ROLE],
    )
    .await;
    apply_documented_order(&catalog, &database).await;

    // The emitted privilege SQL leaves the stable guest ACL role CONNECTION
    // FREE. Asked of the server, not of any Rust shape.
    assert!(
        connect_databases(&catalog, APP_ROLE).await.is_empty(),
        "the emitted privilege SQL granted the stable guest ACL role CONNECT, \
         which --prepare-guest-generation refuses"
    );
    // ... and so is the dispatcher's, since `wamn-0h0g.22.24` cut that family
    // over too. It was the LAST stable-LOGIN principal; the batch now grants
    // CONNECT to nobody, and every consumer is a generation.
    assert!(
        connect_databases(&catalog, DISPATCH_READER_ROLE)
            .await
            .is_empty(),
        "the emitted privilege SQL granted the stable dispatch-reader ACL role \
         CONNECT, which every dispatch-reader generation inherits into every \
         database on the cluster"
    );
    // Ownership converged FIRST and stayed converged.
    let owner: String = catalog
        .query_one(
            "SELECT pg_get_userbyid(datdba)::text FROM pg_database WHERE datname = $1",
            &[&database],
        )
        .await
        .expect("read converged database owner")
        .get(0);
    assert_eq!(owner, "wamn_db_owner");

    // This is the role batch's own post-state, before generation preparation
    // gets any chance to harden it as a side effect.
    let stable_before_prepare = role_state(&catalog, APP_ROLE)
        .await
        .expect("the role batch created the stable guest ACL role");
    assert!(
        !stable_before_prepare.get::<_, bool>("rolcanlogin")
            && !stable_before_prepare.get::<_, bool>("rolinherit")
            && !stable_before_prepare.get::<_, bool>("password_set"),
        "role_sql must itself leave wamn_app NOLOGIN, NOINHERIT and passwordless"
    );

    let target_url = database_url(&admin_url, &database);
    let path_a = secret_path("a");
    provision_project_env::run(prepare_args(&target_url, CredentialGeneration::A, &path_a))
        .await
        .expect("the verb's own next step must accept the state its own priv.sql produced");

    let target = connect(&target_url).await;
    assert_eq!(
        connect_databases(&target, &role_a).await,
        vec![database.clone()],
        "CONNECT belongs to the generation login, which is where prepare grants it"
    );
    let stable = role_state(&target, APP_ROLE)
        .await
        .expect("the stable guest ACL role exists");
    assert!(!stable.get::<_, bool>("rolcanlogin"));
    assert!(!stable.get::<_, bool>("password_set"));
    assert!(
        stable.get::<_, Vec<String>>("connect_databases").is_empty(),
        "the stable guest ACL role stays connection free"
    );

    // The minted credential really authenticates.
    target
        .batch_execute(&authority_derivations_bootstrap_sql())
        .await
        .expect("install the authority derivations");
    let secret: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path_a).expect("read emitted Secret"))
            .expect("emitted Secret is JSON");
    let url_a = secret["stringData"]["url"]
        .as_str()
        .expect("the Secret carries a url")
        .to_string();
    let guest = connect(&url_a).await;
    let who: String = guest
        .query_one("SELECT current_user::text", &[])
        .await
        .expect("authenticate from the emitted Secret")
        .get(0);
    assert_eq!(who, role_a);

    let _ = std::fs::remove_file(&path_a);
}

/// THE NON-ATOMIC HALF, made explicit rather than left to be rediscovered.
///
/// A cluster provisioned by the PRE-FIX privilege SQL carries `CONNECT` on the
/// stable guest ACL role. `--prepare-guest-generation` refuses it — and the
/// refusal is NOT atomic: it fires after the prepare transaction has already
/// committed, so the stable role stays hardened to NOLOGIN and the target
/// generation survives, rolled back to the inactive shape. This arm pins both
/// halves of that contract, and then pins the documented remedy: re-applying
/// the emitted privilege SQL converges the leftover CONNECT away and the retry
/// succeeds.
#[tokio::test]
#[ignore = "requires a disposable PG18 named by WAMN_PROVISIONING_ORDER_PG18_URL"]
async fn a_refused_prepare_leaves_the_state_its_documentation_promises() {
    let Ok(admin_url) = std::env::var("WAMN_PROVISIONING_ORDER_PG18_URL") else {
        eprintln!("skipping provisioning_order_live (set WAMN_PROVISIONING_ORDER_PG18_URL to run)");
        return;
    };
    let database = project_env_database_name(ORG, PROJECT, ENVIRONMENT, INSTANCE);
    let scope = WorkloadRoleScope::Tenant {
        tenant: TENANT,
        database: &database,
    };
    let role_a = workload_generation_role(WorkloadRoleFamily::App, scope, CredentialGeneration::A)
        .expect("App takes a tenant scope");

    let catalog = connect(&admin_url).await;
    reset_cluster(
        &catalog,
        &database,
        &[&role_a, APP_ROLE, DISPATCH_READER_ROLE],
    )
    .await;
    apply_documented_order(&catalog, &database).await;
    // Re-introduce EXACTLY what the pre-fix privilege SQL left behind. This is
    // the only hand-written statement in the file, and it is here because the
    // shipped builders no longer produce it.
    catalog
        .batch_execute(&format!(
            "GRANT CONNECT ON DATABASE \"{database}\" TO \"{APP_ROLE}\""
        ))
        .await
        .expect("seed a pre-cutover environment");

    let target_url = database_url(&admin_url, &database);
    let path_a = secret_path("refused");
    let error =
        provision_project_env::run(prepare_args(&target_url, CredentialGeneration::A, &path_a))
            .await
            .expect_err("a connectable stable ACL role must be refused");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("is not a connection-free NOLOGIN role"),
        "the refusal must name the connection-free posture: {rendered}"
    );

    // THE LEFTOVERS, named exactly. The refusal is not atomic and this is the
    // contract for what survives it.
    let target = connect(&target_url).await;
    let stable = role_state(&target, APP_ROLE)
        .await
        .expect("the stable guest ACL role survives the refusal");
    assert!(
        !stable.get::<_, bool>("rolcanlogin") && !stable.get::<_, bool>("password_set"),
        "the refused prepare LEFT the stable role hardened to NOLOGIN"
    );
    let generation = role_state(&target, &role_a)
        .await
        .expect("the refused prepare LEFT the target generation role in place");
    assert!(
        !generation.get::<_, bool>("rolcanlogin"),
        "the leftover generation is rolled back to the inactive shape"
    );
    assert!(
        generation
            .get::<_, Vec<String>>("connect_databases")
            .is_empty(),
        "the leftover generation holds no CONNECT"
    );
    assert!(
        generation.get::<_, Vec<String>>("memberships").is_empty(),
        "the leftover generation inherits nothing"
    );
    assert!(
        !path_a.exists(),
        "a refused prepare must not publish a Secret"
    );

    // THE DOCUMENTED REMEDY: re-apply the emitted privilege SQL. It converges
    // the stale CONNECT away, and the retry meets the leftovers and succeeds.
    catalog
        .batch_execute(&privilege_sql(&database))
        .await
        .expect("re-apply privilege SQL");
    assert!(
        connect_databases(&catalog, APP_ROLE).await.is_empty(),
        "the emitted privilege SQL must CONVERGE a stale stable-role CONNECT away"
    );
    provision_project_env::run(prepare_args(&target_url, CredentialGeneration::A, &path_a))
        .await
        .expect("the retry meets the partially-prepared cluster and completes");
    assert_eq!(
        connect_databases(&target, &role_a).await,
        vec![database.clone()]
    );

    let _ = std::fs::remove_file(&path_a);
}
