//! Prepare, rotate, retire, and abort workload generations on one project database.
//! A separate case starts with the stale CONNECT grant that prepare must refuse.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tokio_postgres::{Client, NoTls};
use url::Url;

use wamn_control::provision_project_env::{
    self, WorkloadActionRequest, WorkloadActionVerb, WorkloadGenerationAction, privilege_sql,
    role_posture_sql,
};
use wamn_control_provision::tenant_key::{authority_derivations_bootstrap_sql, tenant_key};
use wamn_control_provision::{
    APP_ROLE, CredentialGeneration, DB_OWNER_ROLE, MANAGEMENT_ADMITTER_ROLE, PLATFORM_GROUP_ROLE,
    WorkloadRoleFamily, WorkloadRoleScope, management_admitter_secret_name,
    parse_management_admission_url, project_env_database_name, project_env_guest_secret_name,
    render_management_admitter_secret_manifest, sql, workload_generation_role,
};
use wamn_control_registry::Triple;
use wamn_test_infrastructure::scratch::ScratchRoot;

const ORG: &str = "pg18order";
const PROJECT: &str = "inventory";
const ENVIRONMENT: &str = "dev";
const ENV_KEY: &str = "pg18order--inventory--dev";
const INSTANCE: &str = "k3m9x2p7";

struct Fixture {
    database: String,
    url: String,
    catalog: Client,
    target: Client,
    admin: wamn_test_postgres::Database,
    scratch: ScratchRoot,
    _lock: wamn_test_postgres::ProcessLock,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.admin
            .execute(&[&format!(
                "DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)",
                self.database
            )])
            .expect("drop the project database");
    }
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(connection);
    client
}

fn generation_role(
    family: WorkloadRoleFamily,
    database: &str,
    generation: CredentialGeneration,
) -> String {
    let scope = if family == WorkloadRoleFamily::App {
        WorkloadRoleScope::EnvKey {
            env_key: ENV_KEY,
            database,
        }
    } else {
        WorkloadRoleScope::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: ENVIRONMENT,
            database,
        }
    };
    workload_generation_role(family, scope, generation).unwrap()
}

async fn fixture() -> Fixture {
    let lock = wamn_test_postgres::lock();
    let admin = wamn_test_postgres::database();
    let mut url = Url::parse(admin.url()).unwrap();
    url.set_path("/postgres");
    let catalog = connect(url.as_str()).await;
    let version: i32 = catalog
        .query_one("SHOW server_version_num", &[])
        .await
        .unwrap()
        .get::<_, String>(0)
        .parse()
        .unwrap();
    assert!(version >= 180_000, "credential test requires PostgreSQL 18");
    let database = project_env_database_name(ORG, PROJECT, ENVIRONMENT, INSTANCE);
    catalog
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS \"{database}\" WITH (FORCE)"
        ))
        .await
        .unwrap();
    for family in [
        WorkloadRoleFamily::App,
        WorkloadRoleFamily::ManagementAdmitter,
        WorkloadRoleFamily::Administration,
    ] {
        for generation in [CredentialGeneration::A, CredentialGeneration::B] {
            let role = generation_role(family, &database, generation);
            catalog
                .batch_execute(&format!("DROP ROLE IF EXISTS \"{role}\""))
                .await
                .unwrap();
        }
        catalog
            .batch_execute(&format!("DROP ROLE IF EXISTS \"{}\"", family.acl_role()))
            .await
            .unwrap();
    }
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
            "SET app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'; ALTER TABLE registry.project_envs OWNER TO wamn_system; \
             INSERT INTO registry.orgs (id, placement_type, pool_cluster) \
             VALUES ('{ORG}', 'pooled', 'pool') ON CONFLICT (id) DO NOTHING; \
             INSERT INTO registry.env_policies \
               (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, \
                image, backup_cadence, wal_retention, hibernation) \
             VALUES ('{ORG}', '{ENVIRONMENT}', '{{\"kind\":\"own\"}}', 1, 1, '1Gi', '250m', \
                     '256Mi', 'postgres:18', '', '', 'off') \
             ON CONFLICT (org, name) DO NOTHING; \
             INSERT INTO registry.projects (org, id) VALUES ('{ORG}', '{PROJECT}') \
             ON CONFLICT (org, id) DO NOTHING; \
             INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix, policy_name) \
             VALUES ('{ORG}', '{PROJECT}', '{ENVIRONMENT}', \
                     'wamn-db-{ORG}--{PROJECT}--{ENVIRONMENT}', '{INSTANCE}', '{ENVIRONMENT}') \
             ON CONFLICT (org, project, env) DO UPDATE SET instance_suffix = EXCLUDED.instance_suffix;"
        ))
        .await
        .expect("install stored project-env instance");
    catalog
        .batch_execute(sql::revoke_public_connect_floor_sql())
        .await
        .unwrap();
    apply_documented_order(&catalog, &database).await;
    url.set_path(&format!("/{database}"));
    let url = url.to_string();
    let target = connect(&url).await;
    Fixture {
        database,
        url,
        catalog,
        target,
        admin,
        scratch: ScratchRoot::create().unwrap(),
        _lock: lock,
    }
}

async fn apply_documented_order(catalog: &Client, database: &str) {
    catalog
        .batch_execute(&role_posture_sql())
        .await
        .expect("step 1a: role posture SQL");
    catalog
        .batch_execute(&sql::drain_app_role_sessions_sql())
        .await
        .expect("step 1b: retired shared-login session drain");
    catalog
        .batch_execute(&format!(
            "CREATE DATABASE \"{database}\" OWNER \"{DB_OWNER_ROLE}\""
        ))
        .await
        .expect("step 2: the Database CR stand-in");
    catalog
        .batch_execute(&privilege_sql(database))
        .await
        .expect("step 3: privilege SQL");
}

fn request(
    fixture: &Fixture,
    family: WorkloadRoleFamily,
    verb: WorkloadActionVerb,
    generation: CredentialGeneration,
) -> WorkloadActionRequest {
    let mut system_url = Url::parse(&fixture.url).unwrap();
    system_url.set_path("/postgres");
    WorkloadActionRequest {
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENVIRONMENT.into(),
        system_database_url: Some(system_url.to_string()),
        namespace: if family == WorkloadRoleFamily::ManagementAdmitter {
            "wamn-system".into()
        } else {
            format!("wamn-{ORG}--{PROJECT}--{ENVIRONMENT}--{INSTANCE}")
        },
        target_admin_database_url: Some(fixture.url.clone()),
        cluster: None,
        db_host: system_url.host_str().map(str::to_owned),
        db_port: system_url.port_or_known_default().unwrap_or(5432),
        action: WorkloadGenerationAction {
            family,
            verb,
            generation,
        },
        secret: (verb == WorkloadActionVerb::Prepare)
            .then(|| secret_path(fixture, family, generation)),
        emit_role_sql: None,
        control_administration_patch: None,
    }
}

fn secret_path(
    fixture: &Fixture,
    family: WorkloadRoleFamily,
    generation: CredentialGeneration,
) -> PathBuf {
    fixture.scratch.path().join(format!(
        "{}-{}.json",
        family.acl_role(),
        generation.as_str()
    ))
}

fn secret_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn secret_url(
    fixture: &Fixture,
    family: WorkloadRoleFamily,
    generation: CredentialGeneration,
) -> String {
    let secret = secret_json(&secret_path(fixture, family, generation));
    let url = secret["stringData"]["url"].as_str().unwrap().to_owned();
    if family == WorkloadRoleFamily::ManagementAdmitter {
        assert_eq!(
            secret["metadata"]["name"],
            management_admitter_secret_name(ORG, PROJECT, ENVIRONMENT)
        );
        assert_eq!(secret["stringData"].as_object().unwrap().len(), 1);
        let connection = parse_management_admission_url(&url, ORG, PROJECT, ENVIRONMENT).unwrap();
        assert_eq!(connection.database(), fixture.database);
    }
    url
}

fn state_sql() -> String {
    format!(
        "SELECT state.*, (SELECT rolpassword IS NOT NULL FROM pg_catalog.pg_authid \
             WHERE rolname = $1::text) AS password_set FROM ({}) state",
        sql::workload_generation_state_sql()
    )
}

async fn role_state(admin: &Client, role: &str) -> tokio_postgres::Row {
    admin.query_one(&state_sql(), &[&role]).await.unwrap()
}

// The attributes are LOGIN, INHERIT, password presence, and finite validity.
async fn assert_role(
    admin: &Client,
    role: &str,
    attributes: [bool; 4],
    parents: &[&str],
    children: &[&str],
    databases: &[&str],
) {
    let row = role_state(admin, role).await;
    for (attribute, expected) in [
        "rolcanlogin",
        "rolinherit",
        "password_set",
        "valid_until_finite",
    ]
    .into_iter()
    .zip(attributes)
    {
        assert_eq!(
            row.get::<_, bool>(attribute),
            expected,
            "{role} {attribute}"
        );
    }
    for attribute in [
        "rolsuper",
        "rolcreaterole",
        "rolcreatedb",
        "rolreplication",
        "rolbypassrls",
    ] {
        assert!(!row.get::<_, bool>(attribute), "{role} {attribute}");
    }
    for (column, expected) in [
        ("memberships", parents),
        ("member_roles", children),
        ("connect_databases", databases),
    ] {
        assert_eq!(
            row.get::<_, Vec<String>>(column),
            expected,
            "{role} {column}"
        );
    }
    for column in [
        "membership_options_exact",
        "member_options_exact",
        "generation_children_exact",
    ] {
        assert!(row.get::<_, bool>(column), "{role} {column}");
    }
    assert_eq!(row.get::<_, i64>("owned_objects"), 0, "{role} ownership");
}

async fn login(url: &str, expected: &str) -> Client {
    let client = connect(url).await;
    let who: String = client
        .query_one("SELECT current_user::text", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(who, expected);
    client
}

async fn await_zero_sessions(admin: &Client, role: &str) {
    for _ in 0..100 {
        if role_state(admin, role).await.get::<_, i64>("sessions") == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("{role} still has a live session");
}

async fn direct_acl_set(admin: &Client, role: &str) -> BTreeSet<String> {
    admin
        .query(sql::role_database_grants_sql(), &[&role])
        .await
        .expect("read direct grants")
        .into_iter()
        .map(|row| {
            format!(
                "{}:{}:{}:{}",
                row.get::<_, String>("object_type"),
                row.get::<_, String>("schema_name"),
                row.get::<_, String>("object_name"),
                row.get::<_, String>("privilege_type")
            )
        })
        .collect()
}

async fn family_state(admin: &Client, roles: &[&str]) -> Vec<(String, Vec<String>, Vec<String>)> {
    let mut snapshot = Vec::new();
    for role in roles {
        let row = admin
            .query_opt(&state_sql(), &[role])
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

fn expected_stable_acl() -> BTreeSet<String> {
    let mut expected = BTreeSet::from([
        "schema:catalog:catalog:USAGE".to_string(),
        // The catalog wiring index still needs this exact function grant.
        "routine:wamn_authority:tenant_key:EXECUTE".to_string(),
    ]);
    for relation in sql::MANAGEMENT_ADMITTER_CATALOG_RELATIONS {
        expected.insert(format!("relation:catalog:{relation}:SELECT"));
    }
    expected
}

#[tokio::test]
async fn prepare_rotate_retire_and_abort_preserve_family_authority() {
    let fixture = fixture().await;
    let target = &fixture.target;
    let owner: String = fixture
        .catalog
        .query_one(
            "SELECT pg_get_userbyid(datdba)::text FROM pg_database WHERE datname = $1",
            &[&fixture.database],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(owner, "wamn_db_owner");
    let stable = role_state(target, APP_ROLE).await;
    assert!(!stable.get::<_, bool>("rolcanlogin"));
    assert!(!stable.get::<_, bool>("rolinherit"));
    assert!(!stable.get::<_, bool>("password_set"));
    assert!(stable.get::<_, Vec<String>>("connect_databases").is_empty());
    for family in [
        WorkloadRoleFamily::App,
        WorkloadRoleFamily::ManagementAdmitter,
    ] {
        let role_a = generation_role(family, &fixture.database, CredentialGeneration::A);
        let role_b = generation_role(family, &fixture.database, CredentialGeneration::B);
        let acl = if family == WorkloadRoleFamily::App {
            APP_ROLE
        } else {
            MANAGEMENT_ADMITTER_ROLE
        };
        let parents: &[&str] = if family == WorkloadRoleFamily::App {
            &[]
        } else {
            &[PLATFORM_GROUP_ROLE]
        };
        let pair = [role_a.as_str(), role_b.as_str(), acl];
        assert_ne!(role_a, role_b);
        assert!(!role_a.contains(ENV_KEY));
        provision_project_env::run_workload_action(&request(
            &fixture,
            family,
            WorkloadActionVerb::Prepare,
            CredentialGeneration::A,
        ))
        .await
        .unwrap();
        assert_role(
            target,
            &role_a,
            [true; 4],
            &[acl],
            &[],
            &[&fixture.database],
        )
        .await;
        assert_role(target, acl, [false; 4], parents, &[&role_a], &[]).await;
        let mut url_a = secret_url(&fixture, family, CredentialGeneration::A);

        if family == WorkloadRoleFamily::App {
            // Guest and provisioning-order assertions share this first prepare.
            let actor = wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string();
            wamn_test_infrastructure::record_history::assert_stamp(
                &fixture.catalog,
                "registry.generation_passwords",
                &format!("role = '{role_a}'"),
                &actor,
                &actor,
            )
            .await;
            let key = tenant_key(ENV_KEY, &fixture.database);
            assert!(role_a.contains(&key) && role_b.contains(&key));
            assert!(role_a.len() <= 63 && role_b.len() <= 63);
            let secret = secret_json(&secret_path(&fixture, family, CredentialGeneration::A));
            assert_eq!(
                secret["metadata"]["name"],
                project_env_guest_secret_name(ORG, PROJECT, ENVIRONMENT)
            );
            assert_eq!(secret["metadata"]["labels"]["wamn.tenant-key"], key);
            assert_eq!(
                secret["metadata"]["annotations"]["wamn.io/env_key"],
                ENV_KEY
            );
            target
                .batch_execute(&authority_derivations_bootstrap_sql())
                .await
                .unwrap();
            let guest = login(&url_a, &role_a).await;
            let derived: Option<String> = guest
                .query_one("SELECT wamn_authority.current_tenant_key()", &[])
                .await
                .unwrap()
                .get(0);
            assert_eq!(derived.as_deref(), Some(key.as_str()));
            let matches_row: bool = guest.query_one(
                "SELECT wamn_authority.tenant_key($1::text) = wamn_authority.current_tenant_key()", &[&ENV_KEY],
            ).await.unwrap().get(0);
            assert!(matches_row);
        } else {
            assert!(
                target
                    .query(sql::public_connect_databases_sql(), &[])
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                direct_acl_set(target, &role_a).await,
                BTreeSet::from([format!("database:{0}:{0}:CONNECT", fixture.database)])
            );
            fixture
                .catalog
                .execute(sql::forget_generation_password_sql(), &[&role_a])
                .await
                .unwrap();
            fixture
                .catalog
                .batch_execute(&sql::backfill_generation_passwords_sql())
                .await
                .unwrap();
            let recorded: Vec<String> = fixture
                .catalog
                .query(
                    "SELECT role FROM registry.generation_passwords ORDER BY role",
                    &[],
                )
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.get(0))
                .collect();
            let mut expected = vec![
                generation_role(
                    WorkloadRoleFamily::App,
                    &fixture.database,
                    CredentialGeneration::B,
                ),
                role_a.clone(),
            ];
            expected.sort();
            assert_eq!(recorded, expected);
            assert_eq!(direct_acl_set(target, acl).await, expected_stable_acl());
            drop(login(&url_a, &role_a).await);
            await_zero_sessions(target, &role_a).await;
            let before = family_state(target, &pair).await;
            provision_project_env::run_workload_action(&request(
                &fixture,
                family,
                WorkloadActionVerb::Prepare,
                CredentialGeneration::A,
            ))
            .await
            .unwrap();
            assert_eq!(
                family_state(target, &pair).await,
                before,
                "prepare replay changed the family"
            );
            let replayed = secret_url(&fixture, family, CredentialGeneration::A);
            assert_ne!(
                replayed, url_a,
                "prepare replay must mint a fresh credential"
            );
            drop(login(&replayed, &role_a).await);
            await_zero_sessions(target, &role_a).await;
            url_a = replayed;
        }

        provision_project_env::run_workload_action(&request(
            &fixture,
            family,
            WorkloadActionVerb::Prepare,
            CredentialGeneration::B,
        ))
        .await
        .unwrap();
        let url_b = secret_url(&fixture, family, CredentialGeneration::B);
        assert_ne!(url_b, url_a);
        assert_role(
            target,
            &role_b,
            [true; 4],
            &[acl],
            &[],
            &[&fixture.database],
        )
        .await;
        assert!(
            role_state(target, &role_a)
                .await
                .get::<_, bool>("rolcanlogin"),
            "A remains active during overlap"
        );
        assert_eq!(
            role_state(target, acl)
                .await
                .get::<_, Vec<String>>("member_roles"),
            [role_a.as_str(), role_b.as_str()]
        );
        if family == WorkloadRoleFamily::ManagementAdmitter {
            let before = family_state(target, &pair).await;
            provision_project_env::run_workload_action(&request(
                &fixture,
                family,
                WorkloadActionVerb::Retire,
                CredentialGeneration::A,
            ))
            .await
            .expect_err("retire without a serving replacement refuses");
            assert_eq!(family_state(target, &pair).await, before);
        }
        let serving = login(&url_b, &role_b).await;
        provision_project_env::run_workload_action(&request(
            &fixture,
            family,
            WorkloadActionVerb::Retire,
            CredentialGeneration::A,
        ))
        .await
        .unwrap();
        assert_role(target, &role_a, [false, true, false, true], &[], &[], &[]).await;
        assert_eq!(
            role_state(target, acl)
                .await
                .get::<_, Vec<String>>("member_roles"),
            std::slice::from_ref(&role_b)
        );
        drop(serving);
        await_zero_sessions(target, &role_b).await;

        if family == WorkloadRoleFamily::ManagementAdmitter {
            assert!(direct_acl_set(target, &role_a).await.is_empty());
            assert!(
                tokio_postgres::connect(&url_a, NoTls).await.is_err(),
                "retired URL authenticates"
            );
            let settled = family_state(target, &pair).await;
            provision_project_env::run_workload_action(&request(
                &fixture,
                family,
                WorkloadActionVerb::Retire,
                CredentialGeneration::A,
            ))
            .await
            .expect_err("retire replay refuses");
            assert_eq!(family_state(target, &pair).await, settled);
            provision_project_env::run_workload_action(&request(
                &fixture,
                family,
                WorkloadActionVerb::Abort,
                CredentialGeneration::B,
            ))
            .await
            .unwrap();
            assert_role(target, &role_b, [false, true, false, true], &[], &[], &[]).await;
            assert_role(target, acl, [false; 4], parents, &[], &[]).await;
            assert!(
                tokio_postgres::connect(&url_b, NoTls).await.is_err(),
                "aborted URL authenticates"
            );
            assert_eq!(direct_acl_set(target, acl).await, expected_stable_acl());
            assert_eq!(
                secret_json(&secret_path(&fixture, family, CredentialGeneration::B)),
                render_management_admitter_secret_manifest(
                    &Triple::new(ORG, PROJECT, ENVIRONMENT),
                    "wamn-system",
                    &url_b
                ),
            );
        } else {
            // The next family and administration use the same populated project database.
            target.batch_execute("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles \
                WHERE rolname = 'wamn_scenario_author') THEN \
                CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                NOBYPASSRLS; END IF; END $$; \
                CREATE SCHEMA wamn_run; CREATE SCHEMA unrelated; \
                CREATE TABLE wamn_run.environment_policies (id bigint); \
                CREATE TABLE wamn_run.runs (id bigint); CREATE TABLE wamn_run.run_queue (id bigint); \
                CREATE TABLE wamn_run.untouched (id bigint); CREATE TABLE unrelated.probe (id bigint)").await.unwrap();
            target
                .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
                .await
                .unwrap();
            target
                .batch_execute(include_str!("../../../../deploy/sql/app-schema.sql"))
                .await
                .unwrap();
        }
    }

    // Administration retains its patch refusal, exact patch, and authenticated write.
    let family = WorkloadRoleFamily::Administration;
    let mut prepare = request(
        &fixture,
        family,
        WorkloadActionVerb::Prepare,
        CredentialGeneration::A,
    );
    let refused = provision_project_env::run_workload_action(&prepare)
        .await
        .expect_err("the control patch is required");
    assert!(
        refused
            .to_string()
            .contains("requires --emit-control-administration-patch PATH"),
        "{refused:#}"
    );
    let patch_path = fixture.scratch.path().join("control-patch.json");
    prepare.control_administration_patch = Some(patch_path.clone());
    provision_project_env::run_workload_action(&prepare)
        .await
        .unwrap();
    let secret = secret_json(&secret_path(&fixture, family, CredentialGeneration::A));
    assert_eq!(
        secret_json(&patch_path),
        serde_json::json!({"stringData": {
            (format!("{PROJECT}--{ENVIRONMENT}")): secret["stringData"]["url"].clone()
        }})
    );
    let administration = connect(secret["stringData"]["url"].as_str().unwrap()).await;
    administration.batch_execute(
        "BEGIN; SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000a1', true), \
            set_config('app.operation', 'admin:provisioning-order-fixture', true); \
         INSERT INTO platform.role (name) VALUES ('clerk'); DELETE FROM platform.role WHERE name = 'clerk'; COMMIT;",
    ).await.expect("the administration login writes and appends history");
}

#[tokio::test]
async fn refused_prepare_keeps_inactive_state_and_repairs_on_retry() {
    let fixture = fixture().await;
    fixture
        .catalog
        .batch_execute(&format!(
            "GRANT CONNECT ON DATABASE \"{}\" TO \"{APP_ROLE}\"",
            fixture.database
        ))
        .await
        .unwrap();
    let family = WorkloadRoleFamily::App;
    let prepare = request(
        &fixture,
        family,
        WorkloadActionVerb::Prepare,
        CredentialGeneration::A,
    );
    let error = provision_project_env::run_workload_action(&prepare)
        .await
        .expect_err("the stale stable-role CONNECT must refuse");
    assert!(
        format!("{error:#}").contains("is not a connection-free NOLOGIN role"),
        "{error:#}"
    );
    let stable = role_state(&fixture.target, APP_ROLE).await;
    assert!(!stable.get::<_, bool>("rolcanlogin"));
    assert!(!stable.get::<_, bool>("password_set"));
    let role = generation_role(family, &fixture.database, CredentialGeneration::A);
    let generation = role_state(&fixture.target, &role).await;
    assert!(!generation.get::<_, bool>("rolcanlogin"));
    assert!(
        generation
            .get::<_, Vec<String>>("connect_databases")
            .is_empty()
    );
    assert!(generation.get::<_, Vec<String>>("memberships").is_empty());
    assert!(!secret_path(&fixture, family, CredentialGeneration::A).exists());
    fixture
        .catalog
        .batch_execute(&privilege_sql(&fixture.database))
        .await
        .unwrap();
    assert!(
        role_state(&fixture.target, APP_ROLE)
            .await
            .get::<_, Vec<String>>("connect_databases")
            .is_empty()
    );
    // The clean flow uses the emitted role batch. This retry also tests a missing ACL role.
    fixture
        .catalog
        .batch_execute(&format!("DROP ROLE \"{APP_ROLE}\""))
        .await
        .unwrap();
    provision_project_env::run_workload_action(&prepare)
        .await
        .expect("repair converges the partially prepared state");
    assert_role(&fixture.target, APP_ROLE, [false; 4], &[], &[&role], &[]).await;
    assert_eq!(
        role_state(&fixture.target, &role)
            .await
            .get::<_, Vec<String>>("connect_databases"),
        std::slice::from_ref(&fixture.database)
    );
}
