//! PG18 lifecycle proof for the per-tenant guest-SQL A/B generation pair.
//!
//! `wamn-0h0g.22.6.4`. The generic `wamn-0h0g.13.59` lifecycle is already proven
//! live for two other families, so nothing here re-proves prepare, retire or
//! abort. What is new and unproven is the pairing: the App family with TENANT
//! scope, whose role name carries the very digest
//! `wamn_authority.tenant_key` computes. If those two disagree by one byte,
//! every guest read refuses — and no pure test can see it, because one of the
//! two implementations only exists inside PostgreSQL.
//!
//! Run only against a disposable cluster: the test drops and recreates
//! cluster-global roles, including the stable `wamn_app` ACL role.
//!
//! `WAMN_GUEST_GENERATION_PG18_URL=postgres://.../postgres cargo test -p wamn-ctl \
//!   --test guest_generation_live -- --ignored --nocapture`

use std::path::{Path, PathBuf};

use tokio_postgres::{Client, NoTls};
use url::Url;

use wamn_control_provision::tenant_key::{authority_derivations_bootstrap_sql, tenant_key};
use wamn_control_provision::{
    APP_ROLE, CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope,
    project_env_database_name, project_env_guest_secret_name, sql, workload_generation_role,
};
use wamn_ctl::provision_project_env::{
    self, ProvisionProjectEnvArgs, WorkloadActionVerb, WorkloadGenerationAction,
    WorkloadGenerationArgs,
};

const ORG: &str = "pg18guest";
const PROJECT: &str = "receiving";
const ENVIRONMENT: &str = "dev";
/// A tenant carrying characters a Kubernetes label value would reject and a
/// length that would matter if the name embedded it: the digest is what makes
/// both harmless, so the gate uses a tenant that would expose it otherwise.
const TENANT: &str = "tenant_live-64-0123456789012345678901234567890123456789012345";
const INSTANCE: &str = "k3m9x2p7";

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
        "wamn-guest-pg18-{}-{}.json",
        std::process::id(),
        generation.as_str()
    ))
}

fn secret_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read emitted Secret"))
        .expect("emitted Secret is JSON")
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
        app_host: None,
        app_password: None,
        dispatch_reader_password: None,
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
        // wamn-0h0g.22.16: one derived action, not four families of fields.
        workload: WorkloadGenerationArgs {
            action: prepare
                .map(|(generation, _)| (WorkloadActionVerb::Prepare, generation))
                .or_else(|| retire.map(|generation| (WorkloadActionVerb::Retire, generation)))
                .or_else(|| abort.map(|generation| (WorkloadActionVerb::Abort, generation)))
                .map(|(verb, generation)| WorkloadGenerationAction {
                    family: WorkloadRoleFamily::App,
                    verb,
                    generation,
                }),
            secret: prepare
                .map(|(_, path)| (WorkloadRoleFamily::App, path.to_path_buf())),
        },
    }
}

async fn role_state(admin: &Client, role: &str) -> tokio_postgres::Row {
    admin
        .query_one(sql::workload_generation_state_sql(), &[&role])
        .await
        .expect("read exact role state")
}

/// The attribute floor a guest generation must carry, and nothing beyond it.
async fn assert_guest_attributes(admin: &Client, role: &str, database: &str) {
    let row = role_state(admin, role).await;
    assert!(row.get::<_, bool>("rolcanlogin"), "{role} must be LOGIN");
    assert!(!row.get::<_, bool>("rolsuper"), "{role} NOSUPERUSER");
    assert!(!row.get::<_, bool>("rolcreaterole"), "{role} NOCREATEROLE");
    assert!(!row.get::<_, bool>("rolcreatedb"), "{role} NOCREATEDB");
    assert!(
        !row.get::<_, bool>("rolreplication"),
        "{role} NOREPLICATION"
    );
    assert!(!row.get::<_, bool>("rolbypassrls"), "{role} NOBYPASSRLS");
    assert!(row.get::<_, bool>("password_set"), "{role} authenticates");
    assert!(
        row.get::<_, bool>("valid_until_finite"),
        "{role} must expire"
    );
    // ONLY the stable NOLOGIN ACL role, and the options exact — a generation
    // that could SET ROLE to its ACL parent, or grant that membership onward,
    // would hold authority the ACL role's grants never conferred.
    assert_eq!(
        row.get::<_, Vec<String>>("memberships"),
        vec![APP_ROLE.to_string()],
        "{role} inherits exactly the stable guest ACL role"
    );
    assert!(
        row.get::<_, bool>("membership_options_exact"),
        "{role} membership must carry SET false and ADMIN false"
    );
    assert!(
        row.get::<_, Vec<String>>("member_roles").is_empty(),
        "{role} owns no members of its own"
    );
    assert_eq!(
        row.get::<_, Vec<String>>("connect_databases"),
        vec![database.to_string()],
        "{role} reaches exactly its own project-environment database"
    );
}

#[tokio::test]
#[ignore = "requires a disposable PG18 named by WAMN_GUEST_GENERATION_PG18_URL"]
async fn guest_generations_are_per_tenant_and_carry_the_predicate_key() {
    let Ok(admin_url) = std::env::var("WAMN_GUEST_GENERATION_PG18_URL") else {
        eprintln!("skipping guest_generation_live (set WAMN_GUEST_GENERATION_PG18_URL to run)");
        return;
    };

    let database = project_env_database_name(ORG, PROJECT, ENVIRONMENT, INSTANCE);
    let scope = WorkloadRoleScope::Tenant {
        tenant: TENANT,
        database: &database,
    };
    let role_a = workload_generation_role(WorkloadRoleFamily::App, scope, CredentialGeneration::A)
        .expect("App takes a tenant scope");
    let role_b = workload_generation_role(WorkloadRoleFamily::App, scope, CredentialGeneration::B)
        .expect("App takes a tenant scope");
    let key = tenant_key(TENANT, &database);

    // THE SHARPEST FAILURE MODE, asserted before anything is provisioned: the
    // name the mint will issue carries the key the predicate will compute.
    assert!(role_a.contains(&key), "{role_a} must carry {key}");
    assert!(role_b.contains(&key), "{role_b} must carry {key}");
    assert!(
        !role_a.contains(TENANT),
        "the tenant id must never appear verbatim in a role name"
    );
    assert!(role_a.len() <= 63 && role_b.len() <= 63, "identifier cap");
    assert_ne!(role_a, role_b);

    let catalog = connect(&admin_url).await;
    // HERMETIC PREAMBLE. The stable ACL role is DROPPED, not left healthy: a
    // surviving one satisfies the ensure-ACL builder's IF NOT EXISTS guard and
    // would mask a mutated builder.
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
    catalog
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS \"{database}\" WITH (FORCE)"
        ))
        .await
        .expect("drop prior lifecycle database");
    catalog
        .batch_execute(&format!(
            "DROP ROLE IF EXISTS \"{role_a}\"; DROP ROLE IF EXISTS \"{role_b}\"; \
             DROP ROLE IF EXISTS \"{APP_ROLE}\";"
        ))
        .await
        .expect("reset lifecycle roles");
    catalog
        .batch_execute(&format!("CREATE DATABASE \"{database}\""))
        .await
        .expect("create lifecycle database");
    // The PUBLIC floor only, NOT `grant_connect_on_database_sql`: that helper
    // also grants CONNECT to `wamn_app`, and for THIS family `wamn_app` IS the
    // stable ACL role. Recreating it here would leave a healthy role for the
    // ensure-ACL builder's IF NOT EXISTS guard to find and mask a mutated
    // builder — the trap the drop above exists to avoid. The generation's own
    // CONNECT is granted by the action and asserted below.
    catalog
        .batch_execute(&format!(
            "REVOKE CONNECT, TEMPORARY ON DATABASE \"{database}\" FROM PUBLIC"
        ))
        .await
        .expect("revoke the PUBLIC floor the action requires");

    let target_url = database_url(&admin_url, &database);
    let target = connect(&target_url).await;

    let path_a = secret_path(CredentialGeneration::A);
    let path_b = secret_path(CredentialGeneration::B);

    // ---- prepare A --------------------------------------------------------
    provision_project_env::run(action_args(
        &target_url,
        Some((CredentialGeneration::A, &path_a)),
        None,
        None,
    ))
    .await
    .expect("prepare initial guest generation A");
    assert_guest_attributes(&target, &role_a, &database).await;

    let secret = secret_json(&path_a);
    assert_eq!(
        secret["metadata"]["name"],
        serde_json::json!(project_env_guest_secret_name(ORG, PROJECT, ENVIRONMENT))
    );
    assert_eq!(
        secret["metadata"]["labels"]["wamn.tenant-key"],
        serde_json::json!(key),
        "the Secret label must name the key the predicate computes"
    );
    assert_eq!(
        secret["metadata"]["annotations"]["wamn.io/tenant"],
        serde_json::json!(TENANT),
        "the tenant travels in an annotation, which has no label's charset or \
         length limit"
    );
    let url_a = secret["stringData"]["url"]
        .as_str()
        .expect("the Secret carries a url")
        .to_string();

    // ---- THE LOOP CLOSES HERE --------------------------------------------
    // Install the derivations and ask the SERVER, connected as the minted
    // login, for its own tenant key. This is the one place the provisioning
    // digest and the SQL digest meet.
    target
        .batch_execute(&authority_derivations_bootstrap_sql())
        .await
        .expect("install the authority derivations");
    {
        let guest = connect(&url_a).await;
        let who: String = guest
            .query_one("SELECT current_user::text", &[])
            .await
            .expect("authenticate from the emitted Secret")
            .get(0);
        assert_eq!(who, role_a, "the Secret authenticates as the minted login");
        let derived: Option<String> = guest
            .query_one("SELECT wamn_authority.current_tenant_key()", &[])
            .await
            .expect("derive the session's tenant key")
            .get(0);
        assert_eq!(
            derived.as_deref(),
            Some(key.as_str()),
            "the session derivation and the mint DISAGREE, so every governed \
             read by this login would refuse"
        );
        let matches_row: bool = guest
            .query_one(
                "SELECT wamn_authority.tenant_key($1::text) = wamn_authority.current_tenant_key()",
                &[&TENANT],
            )
            .await
            .expect("evaluate the governed predicate")
            .get(0);
        assert!(matches_row, "the governed predicate must admit this tenant");
    }

    // ---- bounded overlap: prepare B, then retire A ------------------------
    provision_project_env::run(action_args(
        &target_url,
        Some((CredentialGeneration::B, &path_b)),
        None,
        None,
    ))
    .await
    .expect("prepare the replacement guest generation B");
    assert_guest_attributes(&target, &role_b, &database).await;
    assert!(
        role_state(&target, &role_a)
            .await
            .get::<_, bool>("rolcanlogin"),
        "bounded overlap: A still authenticates while B is published"
    );

    // Retirement is USE-PROVEN: the replacement must have a live session, so a
    // generation nobody adopted can never retire the one still in service. Hold
    // B's session open across the retire.
    let url_b = secret_json(&path_b)["stringData"]["url"]
        .as_str()
        .expect("the B Secret carries a url")
        .to_string();
    let replacement = connect(&url_b).await;
    let who_b: String = replacement
        .query_one("SELECT current_user::text", &[])
        .await
        .expect("authenticate B from the emitted Secret")
        .get(0);
    assert_eq!(who_b, role_b);

    provision_project_env::run(action_args(
        &target_url,
        None,
        Some(CredentialGeneration::A),
        None,
    ))
    .await
    .expect("retire the superseded guest generation A");
    drop(replacement);
    let retired = role_state(&target, &role_a).await;
    assert!(
        retired.get::<_, Vec<String>>("memberships").is_empty(),
        "a retired generation inherits nothing"
    );

    for path in [&path_a, &path_b] {
        let _ = std::fs::remove_file(path);
    }
}
