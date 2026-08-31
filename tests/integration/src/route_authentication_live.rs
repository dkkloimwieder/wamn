//! Production route authentication and exact-operation authorization on one
//! fresh disposable PostgreSQL 18 server.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use reqwest::Url;
use serde_json::Value;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION;
use wamn_control_provision::{
    CONTROL_PORTABLE_STORE_SQL, CredentialGeneration, SYSTEM_SCHEMA_SQL, SystemReader,
    WorkloadRoleFamily, parse_system_reader_url, sql as provision_sql,
};
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::provision_project_env::{
    self, ProvisionProjectEnvArgs, WorkloadActionVerb, WorkloadGenerationAction,
    WorkloadGenerationArgs,
};
use wamn_execution_host::authorize_attachment_for_test;
use wamn_platform_identity::{
    PrincipalKind, assign_project_role, create_service, issue_pat, resolve_subject, revoke_pat,
    route_caller_subject,
};
use wamn_runtime::plugins::flow_http_routing::{
    FlowHttpRouting, RouteAuthentication, RouteInFlightLimit,
};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, CredentialProvider, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};
use wamn_runtime::release_manifest::ReleaseManifestWeld;

const URL_ENV: &str = "WAMN_ROUTE_AUTH_PG18_URL";
const ORG: &str = "acme";
const PROJECT: &str = "receiving";
const OTHER_PROJECT: &str = "other";
const ENVIRONMENT: &str = "dev";
const OTHER_ENVIRONMENT: &str = "prod";
const TENANT: &str = "receiving-route-auth";
const ROUTE_CALLER_ROLE: &str = "route-caller";
const ATTACHMENT_ID: &str = "receiving-purchase-order-get";
const OPERATION: &str = "wamn_receiving@1.0.0::purchase_order.get";
const RESIDUE: &str = "wamn_receiving@1.0.0::obsolete.operation";

#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    Authentication(u16, String),
    Permission(Box<str>),
}

struct ProvisionedRoute {
    database_url: String,
    token: String,
    token_prefix: String,
    principal_subject: String,
}

struct ScratchRoot(PathBuf);

impl ScratchRoot {
    fn create() -> anyhow::Result<Self> {
        let path = scratch_root();
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).context("create route-auth proof directory")?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn connect(url: &str) -> anyhow::Result<(Arc<Client>, tokio::task::JoinHandle<()>)> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to disposable PostgreSQL")?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((Arc::new(client), task))
}

fn database_url(admin_url: &str, database: &str) -> anyhow::Result<String> {
    let mut url = Url::parse(admin_url).context("parse disposable PostgreSQL URL")?;
    url.set_path(&format!("/{database}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

fn scratch_root() -> PathBuf {
    std::env::temp_dir().join(format!("route-authentication-live-{}", std::process::id()))
}

fn provisioning_args(
    system_url: &str,
    root: &Path,
    route_secret: &Path,
) -> ProvisionProjectEnvArgs {
    ProvisionProjectEnvArgs {
        org: Some(ORG.to_owned()),
        project: Some(PROJECT.to_owned()),
        env: Some(ENVIRONMENT.to_owned()),
        tenant: Some(TENANT.to_owned()),
        system_database_url: Some(system_url.to_owned()),
        cluster: Some("route-auth-pg18".to_owned()),
        connection_limit: None,
        app_password: Some("unused-legacy-secret".to_owned()),
        app_host: Some("route-auth-pg18.invalid".to_owned()),
        app_port: 5432,
        namespace: "wamn-system".to_owned(),
        secret_namespace: None,
        target_admin_database_url: None,
        workload: WorkloadGenerationArgs::default(),
        emit_database: Some(root.join("database.json")),
        emit_role_sql: Some(root.join("roles.sql")),
        emit_privilege_sql: Some(root.join("privileges.sql")),
        emit_secret: Some(root.join("database-secret.json")),
        emit_management_author_pat_secret: None,
        emit_route_caller_pat_secret: Some(route_secret.to_path_buf()),
        revoke_pat_prefix: None,
    }
}

fn generation_args(
    family: WorkloadRoleFamily,
    system_url: &str,
    target_admin_url: Option<&str>,
    secret: &Path,
) -> ProvisionProjectEnvArgs {
    ProvisionProjectEnvArgs {
        org: Some(ORG.to_owned()),
        project: Some(PROJECT.to_owned()),
        env: Some(ENVIRONMENT.to_owned()),
        tenant: Some(TENANT.to_owned()),
        system_database_url: Some(system_url.to_owned()),
        cluster: None,
        connection_limit: None,
        app_password: None,
        app_host: None,
        app_port: 5432,
        namespace: "wamn-system".to_owned(),
        secret_namespace: None,
        target_admin_database_url: target_admin_url.map(str::to_owned),
        workload: WorkloadGenerationArgs {
            action: Some(WorkloadGenerationAction {
                family,
                verb: WorkloadActionVerb::Prepare,
                generation: CredentialGeneration::A,
            }),
            secret: Some((family, secret.to_path_buf())),
        },
        emit_database: None,
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: None,
        emit_management_author_pat_secret: None,
        emit_route_caller_pat_secret: None,
        revoke_pat_prefix: None,
    }
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}

fn secret_value(path: &Path, key: &str) -> anyhow::Result<String> {
    read_json(path)?["stringData"][key]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{} carries stringData.{key}", path.display()))
}

fn secret_annotation(path: &Path, key: &str) -> anyhow::Result<String> {
    read_json(path)?["metadata"]["annotations"][key]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{} carries annotation {key}", path.display()))
}

async fn reset_and_install_control(admin: &Client) -> anyhow::Result<()> {
    let stale_databases = admin
        .query(
            "SELECT datname::text FROM pg_database \
             WHERE datname LIKE 'wamn-db-acme--%--%--%' ORDER BY datname",
            &[],
        )
        .await
        .context("list stale route-auth databases")?;
    for row in stale_databases {
        let database: String = row.get(0);
        admin
            .batch_execute(&provision_sql::drop_database_named_sql(&database))
            .await
            .with_context(|| format!("drop stale database {database}"))?;
    }
    admin
        .batch_execute(
            "DROP SCHEMA IF EXISTS identity CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
                 CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$; \
             DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
                                        current_database()); END $$;",
        )
        .await
        .context("prepare the production control owner")?;
    admin
        .batch_execute(&provision_sql::ensure_control_author_acl_role_sql())
        .await
        .context("ensure the portable store's control-author ACL role")?;
    admin
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("assume the production control owner")?;
    admin
        .batch_execute(SYSTEM_SCHEMA_SQL)
        .await
        .context("install deploy/sql/system-schema.sql")?;
    admin
        .batch_execute(CONTROL_PORTABLE_STORE_SQL)
        .await
        .context("install the control portable store")?;
    admin
        .batch_execute(
            r#"RESET ROLE;
               SET ROLE wamn_system;
               INSERT INTO registry.orgs (id, placement_kind, pool_cluster)
               VALUES ('acme', 'pooled', 'route-auth-pg18');
               INSERT INTO registry.env_policies
                 (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
               VALUES
                 ('acme', 'dev', '"own"'::jsonb, 1, 1, '1Gi', '1', '1Gi', 'postgres:18'),
                 ('acme', 'prod', '"own"'::jsonb, 2, 1, '1Gi', '1', '1Gi', 'postgres:18');
               RESET ROLE;"#,
        )
        .await
        .context("seed the declared environment policies")?;
    admin
        .batch_execute(provision_sql::revoke_public_connect_floor_sql())
        .await
        .context("converge the cluster PUBLIC CONNECT floor")?;
    admin
        .batch_execute(
            "DO $$ BEGIN EXECUTE format(\
               'REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', current_database()); END $$;",
        )
        .await
        .context("converge the control database PUBLIC TEMPORARY floor")?;
    Ok(())
}

async fn provision_route(
    system_url: &str,
    admin: &Client,
    root: &Path,
) -> anyhow::Result<ProvisionedRoute> {
    let route_secret = root.join("route-caller-pat.json");
    provision_project_env::run(provisioning_args(system_url, root, &route_secret))
        .await
        .context("run production project-environment and route-PAT provisioning")?;

    let database = read_json(&root.join("database.json"))?["spec"]["name"]
        .as_str()
        .context("Database CR carries spec.name")?
        .to_owned();
    admin
        .batch_execute(
            &std::fs::read_to_string(root.join("roles.sql")).context("read emitted role SQL")?,
        )
        .await
        .context("apply emitted role SQL")?;
    admin
        .batch_execute(&wamn_schema_control::ensure_scenario_author_role_sql())
        .await
        .context("ensure the catalog author role")?;
    admin
        .batch_execute(&provision_sql::create_database_named_sql(&database))
        .await
        .context("stand in for the emitted Database CR")?;
    admin
        .batch_execute(
            &std::fs::read_to_string(root.join("privileges.sql"))
                .context("read emitted privilege SQL")?,
        )
        .await
        .context("apply emitted privilege SQL")?;

    Ok(ProvisionedRoute {
        database_url: database_url(system_url, &database)?,
        token: secret_value(&route_secret, "token")?,
        token_prefix: secret_annotation(&route_secret, "wamn.io/pat-prefix")?,
        principal_subject: secret_annotation(&route_secret, "wamn.io/principal-subject")?,
    })
}

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/receiving")
}

async fn permission_write_identity(project: &Client) -> anyhow::Result<Vec<String>> {
    Ok(project
        .query(
            "SELECT permission || ':' || xmin::text FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 ORDER BY permission COLLATE \"C\"",
            &[&TENANT, &ROUTE_CALLER_ROLE],
        )
        .await
        .context("read permission write identities")?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect())
}

async fn install_project_and_reconcile(project: &Client, project_url: &str) -> anyhow::Result<()> {
    project
        .batch_execute(include_str!("../../../deploy/sql/catalog-schema.sql"))
        .await
        .context("install catalog schema")?;
    project
        .batch_execute(include_str!("../../../deploy/sql/app-schema.sql"))
        .await
        .context("install application authorization schema")?;
    project
        .batch_execute("CREATE SCHEMA wamn_run AUTHORIZATION postgres")
        .await
        .context("create the empty run-plane revoke scope")?;
    project
        .execute(
            "INSERT INTO app_system.roles (tenant_id, name, is_system) \
             VALUES ($1, $2, false)",
            &[&TENANT, &ROUTE_CALLER_ROLE],
        )
        .await
        .context("seed the route-caller role")?;
    project
        .execute(
            "INSERT INTO app_system.permissions (tenant_id, role_name, permission) \
             VALUES ($1, $2, $3)",
            &[&TENANT, &ROUTE_CALLER_ROLE, &RESIDUE],
        )
        .await
        .context("seed package-coordinate residue")?;

    let args = || ApplyPackageArgs {
        package: package_root(),
        database_url: project_url.to_owned(),
        tenant: TENANT.to_owned(),
    };
    apply_package::run(args())
        .await
        .context("apply the Receiving package")?;
    let expected = wamn_control_provision::operation_grants::operation_grant_tokens(
        include_bytes!("../../../packages/receiving/wamn.json"),
    )
    .context("derive the strict manifest's operation tokens")?;
    let observed = project
        .query(
            "SELECT permission::text FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 ORDER BY permission COLLATE \"C\"",
            &[&TENANT, &ROUTE_CALLER_ROLE],
        )
        .await
        .context("read reconciled operation grants")?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        observed, expected,
        "the real reconciler must author the manifest set"
    );
    assert_eq!(
        observed.len(),
        6,
        "Receiving declares exactly six operations"
    );
    assert!(
        !observed.contains(RESIDUE),
        "coordinate residue survived reconcile"
    );

    let before = permission_write_identity(project).await?;
    apply_package::run(args())
        .await
        .context("replay the converged Receiving package")?;
    assert_eq!(permission_write_identity(project).await?, before);
    Ok(())
}

fn serving_weld() -> anyhow::Result<Arc<ReleaseManifestWeld>> {
    let definition = serde_json::json!({
        "id": ATTACHMENT_ID,
        "kind": "http",
        "route": {
            "host": "receiving.example.test",
            "path": "/purchase-orders/get",
            "method": "POST"
        }
    });
    let definition_hash = wamn_execution_contract::canonical_json_sha256(&definition);
    let manifest = serde_json::json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {
            "tenant-id": TENANT,
            "effective-release-id": 1,
            "environment": ENVIRONMENT,
            "packages": [{"package-id": "wamn_receiving", "package-version": "1.0.0"}]
        },
        "components": [{
            "package-id": "wamn_receiving",
            "component": "purchase-order-get",
            "interface-version": "0.1.0",
            "digest": format!("sha256:{}", "a".repeat(64)),
            "registered-operation": OPERATION
        }],
        "wirings": [{
            "package-id": "wamn_receiving",
            "wiring-id": "purchase-order-get",
            "wiring-version": 1,
            "graph-hash": format!("sha256:{}", "b".repeat(64))
        }],
        "attachments": {
            (ATTACHMENT_ID): {
                "kind": "http",
                "package-id": "wamn_receiving",
                "wiring-id": "purchase-order-get",
                "wiring-version": 1,
                "definition-hash": definition_hash,
                "definition": definition,
                "auth-policy": {"mode": "pat"},
                "registered-operation": OPERATION
            }
        },
        "registrations": {}
    });
    let bytes = wamn_execution_contract::canonical_json_bytes(&manifest);
    Ok(Arc::new(ReleaseManifestWeld::load_canonical_bytes(
        &bytes,
        "route-authentication-live fixture",
    )?))
}

fn project_postgres(class: AuthorityClass, url: &str) -> anyhow::Result<Arc<WamnPostgres>> {
    let base = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 2,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 100,
    };
    let configuration = serde_json::json!({
        PROJECT: {"credentials": {(class.as_str()): url}}
    });
    let projects = StaticCredentialProvider::projects_from_json(&configuration.to_string(), &base)?;
    let provider: Arc<dyn CredentialProvider> =
        Arc::new(StaticCredentialProvider::new(projects, None));
    Ok(Arc::new(WamnPostgres::with_provider(provider)))
}

fn routing(
    identity_reader: Arc<Client>,
    postgres: Arc<WamnPostgres>,
    weld: Arc<ReleaseManifestWeld>,
) -> anyhow::Result<FlowHttpRouting> {
    Ok(
        FlowHttpRouting::new(Some(weld), RouteInFlightLimit::default()).with_authentication(
            Arc::new(RouteAuthentication::new(
                identity_reader,
                postgres,
                ORG,
                PROJECT,
                route_caller_subject(ORG, PROJECT, ENVIRONMENT)?,
            )),
        ),
    )
}

async fn invoke(
    routing: &FlowHttpRouting,
    weld: &ReleaseManifestWeld,
    authorization: Option<&str>,
    router_admissions: &mut usize,
) -> Result<(), Refusal> {
    let caller = routing
        .authenticate_authorization_for_test(ATTACHMENT_ID, authorization)
        .await
        .map_err(|(status, code)| Refusal::Authentication(status, code))?;
    authorize_attachment_for_test(weld, ATTACHMENT_ID, caller.as_ref())
        .map_err(Refusal::Permission)?;
    *router_admissions += 1;
    Ok(())
}

async fn issue_scoped_token(
    admin: &Client,
    project: &str,
    environment: &str,
) -> anyhow::Result<String> {
    let subject = route_caller_subject(ORG, project, environment)?;
    let principal = create_service(
        admin,
        &subject,
        &format!("route caller {project}/{environment}"),
    )
    .await
    .context("create wrong-scope route caller")?;
    assign_project_role(admin, principal.id(), ORG, project, ROUTE_CALLER_ROLE)
        .await
        .context("assign wrong-scope route-caller role")?;
    Ok(issue_pat(
        admin,
        principal.id(),
        "route-caller",
        Duration::from_secs(3600),
    )
    .await
    .context("issue wrong-scope route PAT")?
    .token()
    .to_owned())
}

async fn issue_pat_for_subject(
    admin: &Client,
    subject: &str,
    label: &str,
) -> anyhow::Result<(String, String)> {
    let principal = resolve_subject(admin, PrincipalKind::Service, subject)
        .await
        .context("resolve route-caller principal")?
        .context("route-caller principal is absent")?;
    let issued = issue_pat(admin, principal.id(), label, Duration::from_secs(3600))
        .await
        .with_context(|| format!("issue {label} PAT"))?;
    Ok((
        issued.token().to_owned(),
        issued.record().prefix().to_owned(),
    ))
}

fn flip_last_hex_digit(token: &str) -> String {
    let (head, last) = token.split_at(token.len() - 1);
    let replacement = if last == "a" { 'b' } else { 'a' };
    format!("{head}{replacement}")
}

#[tokio::test]
#[ignore = "requires a fresh disposable PG18 named by WAMN_ROUTE_AUTH_PG18_URL"]
async fn production_route_caller_authentication_and_operation_authorization() {
    let admin_url = std::env::var(URL_ENV)
        .expect("WAMN_ROUTE_AUTH_PG18_URL must name a fresh disposable PostgreSQL 18 server");
    let scratch = ScratchRoot::create().expect("create route-auth proof directory");
    let root = scratch.path();

    let (admin, admin_task) = connect(&admin_url).await.expect("connect admin");
    let version: i32 = admin
        .query_one("SHOW server_version_num", &[])
        .await
        .expect("read PostgreSQL version")
        .get::<_, String>(0)
        .parse()
        .expect("parse PostgreSQL version");
    assert!(
        version >= 180_000,
        "the gate requires PostgreSQL 18 or newer"
    );
    reset_and_install_control(&admin)
        .await
        .expect("install the control plane");
    let route = provision_route(&admin_url, &admin, &root)
        .await
        .expect("mint the production route caller");
    assert_eq!(
        route.principal_subject,
        route_caller_subject(ORG, PROJECT, ENVIRONMENT).expect("derive expected route subject")
    );

    let (project, project_task) = connect(&route.database_url)
        .await
        .expect("connect project database");
    install_project_and_reconcile(&project, &route.database_url)
        .await
        .expect("install and reconcile Receiving");

    let identity_secret = root.join("identity-reader.json");
    provision_project_env::run(generation_args(
        WorkloadRoleFamily::IdentityReader,
        &admin_url,
        None,
        &identity_secret,
    ))
    .await
    .expect("prepare the production identity-reader generation");
    let http_secret = root.join("http-admitter.json");
    provision_project_env::run(generation_args(
        WorkloadRoleFamily::HttpAdmitter,
        &admin_url,
        Some(&route.database_url),
        &http_secret,
    ))
    .await
    .expect("prepare the production callable-HTTP generation");

    let identity_url = secret_value(&identity_secret, "url").expect("read identity-reader URL");
    parse_system_reader_url(
        SystemReader::Identity,
        &identity_url,
        ORG,
        PROJECT,
        ENVIRONMENT,
    )
    .expect("the identity-reader Secret passes its consumer's exact scope gate");
    let (identity_reader, identity_task) = connect(&identity_url)
        .await
        .expect("connect exact identity-reader generation");
    let http_url = secret_value(&http_secret, "url").expect("read callable-HTTP URL");
    let weld = serving_weld().expect("load the canonical serving weld");
    let route_auth = routing(
        Arc::clone(&identity_reader),
        project_postgres(AuthorityClass::CallableHttp, &http_url)
            .expect("build project-specific callable-HTTP provider"),
        Arc::clone(&weld),
    )
    .expect("build route authentication");

    let mut router_admissions = 0;
    let valid = format!("Bearer {}", route.token);
    invoke(&route_auth, &weld, Some(&valid), &mut router_admissions)
        .await
        .expect("production-minted caller reaches the production router authorization boundary");
    assert_eq!(router_admissions, 1);

    let forged = format!("Bearer {}", flip_last_hex_digit(&route.token));
    let expired = issue_pat_for_subject(&admin, &route.principal_subject, "expired")
        .await
        .expect("mint expiring PAT");
    admin
        .execute(
            "UPDATE identity.pats SET created_at = now() - interval '2 hours', \
             expires_at = now() - interval '1 hour' WHERE token_prefix = $1",
            &[&expired.1],
        )
        .await
        .expect("expire PAT in the server clock");
    let revoked = issue_pat_for_subject(&admin, &route.principal_subject, "revoked")
        .await
        .expect("mint revocable PAT");
    revoke_pat(admin.as_ref(), &revoked.1)
        .await
        .expect("revoke PAT");
    admin
        .execute(
            "INSERT INTO registry.projects (org, id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            &[&ORG, &OTHER_PROJECT],
        )
        .await
        .expect("seed wrong-project role scope");
    let wrong_project = issue_scoped_token(&admin, OTHER_PROJECT, ENVIRONMENT)
        .await
        .expect("mint wrong-project PAT");
    let wrong_environment = issue_scoped_token(&admin, PROJECT, OTHER_ENVIRONMENT)
        .await
        .expect("mint wrong-environment PAT");
    let missing_role = issue_pat_for_subject(&admin, &route.principal_subject, "missing-role")
        .await
        .expect("mint missing-role PAT");
    admin
        .execute(
            "DELETE FROM identity.project_roles WHERE principal_id = \
               (SELECT id FROM identity.principals WHERE kind = 'service' AND subject = $1) \
               AND org = $2 AND project = $3 AND role = $4",
            &[&route.principal_subject, &ORG, &PROJECT, &ROUTE_CALLER_ROLE],
        )
        .await
        .expect("remove the route-caller role");

    let unauthorized = Refusal::Authentication(401, "unauthorized".to_owned());
    for (label, authorization) in [
        ("absent", None),
        ("malformed", Some("Bearer malformed".to_owned())),
        ("forged", Some(forged)),
        ("expired", Some(format!("Bearer {}", expired.0))),
        ("revoked", Some(format!("Bearer {}", revoked.0))),
        ("wrong-project", Some(format!("Bearer {wrong_project}"))),
        (
            "wrong-environment",
            Some(format!("Bearer {wrong_environment}")),
        ),
        ("missing-role", Some(format!("Bearer {}", missing_role.0))),
    ] {
        assert_eq!(
            invoke(
                &route_auth,
                &weld,
                authorization.as_deref(),
                &mut router_admissions,
            )
            .await
            .expect_err(label),
            unauthorized,
            "{label} disclosed a credential-state distinction"
        );
        assert_eq!(router_admissions, 1, "{label} reached router admission");
    }

    let principal = resolve_subject(
        admin.as_ref(),
        PrincipalKind::Service,
        &route.principal_subject,
    )
    .await
    .expect("resolve route caller")
    .expect("route caller remains stored");
    assign_project_role(
        admin.as_ref(),
        principal.id(),
        ORG,
        PROJECT,
        ROUTE_CALLER_ROLE,
    )
    .await
    .expect("restore route-caller role");
    project
        .execute(
            "DELETE FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 AND permission = $3",
            &[&TENANT, &ROUTE_CALLER_ROLE, &OPERATION],
        )
        .await
        .expect("remove the exact operation grant");
    assert_eq!(
        invoke(&route_auth, &weld, Some(&valid), &mut router_admissions,)
            .await
            .expect_err("missing permission must refuse"),
        Refusal::Permission(OPERATION.into())
    );
    assert_eq!(
        router_admissions, 1,
        "missing permission reached router admission"
    );

    let permission_backend_unavailable = routing(
        Arc::clone(&identity_reader),
        project_postgres(
            AuthorityClass::ExecutorPlatform,
            "postgresql://unused.invalid/unused",
        )
        .expect("build provider missing the callable-HTTP credential"),
        Arc::clone(&weld),
    )
    .expect("build permission-unavailable routing");
    assert_eq!(
        invoke(
            &permission_backend_unavailable,
            &weld,
            Some(&valid),
            &mut router_admissions,
        )
        .await
        .expect_err("missing permission authority must be availability"),
        Refusal::Authentication(503, "authentication-unavailable".to_owned())
    );
    assert_eq!(
        router_admissions, 1,
        "permission outage reached router admission"
    );

    identity_task.abort();
    let _ = identity_task.await;
    assert_eq!(
        invoke(&route_auth, &weld, Some(&valid), &mut router_admissions,)
            .await
            .expect_err("identity outage must be availability"),
        Refusal::Authentication(503, "authentication-unavailable".to_owned())
    );
    assert_eq!(
        router_admissions, 1,
        "identity outage reached router admission"
    );

    assert_eq!(route.token_prefix.len(), 16);
    drop(project);
    project_task.abort();
    admin_task.abort();
}
