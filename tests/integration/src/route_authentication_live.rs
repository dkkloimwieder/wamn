//! Production route authentication and shared test-document tests.

use sqlx::{Connection as _, Row as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use wamn_broker::{Publisher, auth_changes::AuthDrops};
use wamn_identity_protocol::ADMIN_ROLE;

use anyhow::Context as _;
use sqlx::PgConnection as Client;
use wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION;
use wamn_control_provision::WorkloadRoleFamily;
use wamn_engine::flow_http_routing::FlowHttpRouting;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::authorize_attachment_for_test;
use wamn_environment_control::apply_package::{self, ApplyPackageRequest};
use wamn_platform_identity::{
    PrincipalType, assign_project_role, create_service, create_user, disable_principal, issue_pat,
    operator_subject, resolve_subject, revoke_pat,
};
use wamn_runtime::plugins::route_authentication::{
    PlatformRouteAuthenticator, RouteAuthentication, SessionRouteAuthentication,
};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, CredentialProvider, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};

use crate::fixture_membership::{self, MembershipRequest};
use wamn_dev_control::dev::environment::{
    ENVIRONMENT, JourneyTarget, ORG, PROJECT, PROJECT_ENV_KEY, connect, provision_route,
    reset_control_store,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

use crate::session_fixture;

const OTHER_PROJECT: &str = "other";
const OTHER_ENVIRONMENT: &str = "prod";
const FIXTURE_PACKAGE_ID: &str = "route_auth_fixture";
const FIXTURE_PACKAGE_VERSION: &str = "1.0.0";
const ATTACHMENT_ID: &str = "route-auth-item-get";
const OPERATION: &str = "route-auth-fixture:item/get@1.0.0";
const FIXTURE_COMPONENT: &str = "route_auth_fixture";
/// The stable reference of [`OPERATION`] that an authored role stores.
const OPERATION_REFERENCE: &str = "route-auth-fixture:item/get";
/// An authored role that holds [`OPERATION_REFERENCE`]. The operator service
/// holds it, so a removed permission row refuses the route.
const ITEM_READER: &str = "item-reader";
/// A stored reference that the release does not serve.
const UNSERVED: &str = "route-auth-fixture:obsolete/operation";
/// A second operation the fixture release serves, held through another role.
const EXTRA_OPERATION: &str = "route-auth-fixture:item/extra@1.0.0";
/// The test principal that the project fixture writes as.
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Refusal {
    Authentication(u16, String),
    Permission(Box<str>),
}

async fn reset_and_install_control(admin: &mut Client) -> anyhow::Result<()> {
    reset_control_store(admin).await?;
    // The fixture writes the policies, principals and tokens as wamn:provisioning.
    sqlx::query("SELECT set_config('app.user_id', $1, false)")
        .bind(
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .execute(&mut *admin)
        .await
        .map(|result| result.rows_affected())
        .context("bind wamn:provisioning for the fixture session")?;
    sqlx::raw_sql(r#"RESET ROLE;
               SET ROLE wamn_system;
               INSERT INTO registry.orgs (id, placement_type, pool_cluster)
               VALUES ('acme', 'pooled', 'route-auth-pg18');
               INSERT INTO registry.env_policies
                 (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
               VALUES
                 ('acme', 'dev', '"own"'::jsonb, 1, 1, '1Gi', '1', '1Gi', 'postgres:18'),
                 ('acme', 'prod', '"own"'::jsonb, 2, 1, '1Gi', '1', '1Gi', 'postgres:18');
               RESET ROLE;"#).execute(&mut *admin).await.map(drop)
        .context("seed the auth-only test's declared environment policies")?;
    Ok(())
}

/// Write the operator service's project `user` row, as `reconcile-run-plane`
/// does, and grant its authored role through the application mutation owner.
async fn grant_service_operator(
    system_url: &str,
    project: &mut Client,
    _project_url: &str,
    subject: &str,
) -> anyhow::Result<()> {
    let mut system = connect(system_url).await?;
    let principal = resolve_subject(&mut system, PrincipalType::Service, subject)
        .await
        .context("resolve the operator service")?
        .context("the operator service is absent")?;
    let email = format!("{subject}@example.invalid");
    sqlx::query(
        "INSERT INTO platform.\"user\" (id, type, email) \
             VALUES ($1::text::uuid, 'service', $2)",
    )
    .bind(&principal.id().to_string().as_str())
    .bind(&email)
    .execute(&mut *project)
    .await
    .map(|result| result.rows_affected())
    .context("write the operator service's user row")?;
    sqlx::raw_sql("BEGIN")
        .execute(&mut *project)
        .await
        .map(drop)?;
    let outcome = wamn_platform_identity::application::mutate_grant(
        project,
        PROJECT_ENV_KEY,
        wamn_platform_identity::application::GrantAuthority::Provisioning,
        wamn_platform_identity::application::GrantChange::UserRole {
            user_id: principal.id().to_string().as_str(),
            role: ITEM_READER,
            grant: true,
        },
        &mut AuthDrops::default(),
    )
    .await?;
    sqlx::raw_sql("COMMIT")
        .execute(&mut *project)
        .await
        .map(drop)?;
    anyhow::ensure!(outcome.changed, "the first grant wrote no row");
    Ok(())
}

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/route_auth_package")
}

async fn permission_write_identity(project: &mut Client) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query(
        "SELECT role_name || ':' || permission || ':' || xmin::text FROM platform.permission \
             ORDER BY role_name, permission COLLATE \"C\"",
    )
    .fetch_all(&mut *project)
    .await
    .context("read permission write identities")?
    .into_iter()
    .map(|row| row.get::<String, _>(0))
    .collect())
}

async fn install_project_and_reconcile(
    project: &mut Client,
    project_url: &str,
) -> anyhow::Result<()> {
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut *project)
        .await
        .map(drop)
        .context("install catalog schema")?;
    sqlx::raw_sql(include_str!("../../../deploy/sql/app-schema.sql"))
        .execute(&mut *project)
        .await
        .map(drop)
        .context("install application authorization schema")?;
    sqlx::raw_sql("CREATE SCHEMA wamn_run AUTHORIZATION postgres")
        .execute(&mut *project)
        .await
        .map(drop)
        .context("create the empty run-plane revoke target")?;
    // The fixture writes as its test principal, whose row stamps itself. Both
    // bindings stay on this session for the later role and permission changes.
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:route-authentication-fixture', false)",
    )
    .bind(&FIXTURE_PRINCIPAL)
    .execute(&mut *project)
    .await
    .map(|result| result.rows_affected())
    .context("bind the fixture test principal")?;
    sqlx::query(
        "INSERT INTO platform.\"user\" (id, type, email) \
             VALUES ($1::text::uuid, 'user', 'fixture@example.invalid')",
    )
    .bind(&FIXTURE_PRINCIPAL)
    .execute(&mut *project)
    .await
    .map(|result| result.rows_affected())
    .context("seed the fixture test principal")?;
    sqlx::query("INSERT INTO platform.role (name) VALUES ($1)")
        .bind(&ITEM_READER)
        .execute(&mut *project)
        .await
        .map(|result| result.rows_affected())
        .context("seed the authored role")?;
    sqlx::query(
        "INSERT INTO platform.permission (role_name, permission) \
             VALUES ($1, $2), ($1, $3)",
    )
    .bind(&ITEM_READER)
    .bind(&OPERATION_REFERENCE)
    .bind(&UNSERVED)
    .execute(&mut *project)
    .await
    .map(|result| result.rows_affected())
    .context("seed the authored role's references")?;

    let scratch = ScratchRoot::create()?;
    let package = scratch.path().join("package");
    let source = package_root();
    std::fs::create_dir_all(package.join("migrations"))?;
    std::fs::create_dir_all(package.join("generated"))?;
    std::fs::copy(
        source.join("generated/wamn.json"),
        package.join("generated/wamn.json"),
    )?;
    for entry in std::fs::read_dir(source.join("migrations"))? {
        let entry = entry?;
        std::fs::copy(
            entry.path(),
            package.join("migrations").join(entry.file_name()),
        )?;
    }
    wamn_test_infrastructure::fixture_digest::write_build_record(&package);
    let args = || ApplyPackageRequest {
        component_source: wamn_environment_control::apply_package::component_source_from_env(None)
            .unwrap(),
        changes: wamn_broker::Publisher::default(),
        package: wamn_environment_control::apply_package::PackageRoot {
            path: package.clone(),
            artifact_digest: Some(
                wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(&package)
                    .expect("read the fixture package root"),
            ),
        },
        database_url: project_url.to_owned(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        environment: ENVIRONMENT.to_owned(),
    };
    let before = permission_write_identity(project).await?;
    apply_package::apply_package(args())
        .await
        .context("apply the route-authentication fixture package")?;
    let roles = sqlx::query("SELECT name FROM platform.role ORDER BY name")
        .fetch_all(&mut *project)
        .await
        .context("read the roles")?
        .into_iter()
        .map(|row| row.get::<String, _>(0))
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        [ADMIN_ROLE, ITEM_READER],
        "apply-package must write admin beside the authored role"
    );
    assert_eq!(
        permission_write_identity(project).await?,
        before,
        "apply-package wrote a permission row"
    );

    apply_package::apply_package(args())
        .await
        .context("replay the converged route-authentication fixture package")?;
    assert_eq!(permission_write_identity(project).await?, before);
    Ok(())
}

fn load_serving_release() -> anyhow::Result<Arc<LoadedRelease>> {
    let definition = serde_json::json!({
        "id": ATTACHMENT_ID,
        "type": "http",
        "route": {
            "host": "receiving.example.test",
            "path": "/purchase-orders/get",
            "method": "POST"
        }
    });
    let definition_hash = wamn_digest::canonical_json_sha256(&definition);
    let manifest = serde_json::json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {
            "packages": [{"package-id": FIXTURE_PACKAGE_ID, "package-version": FIXTURE_PACKAGE_VERSION}]
        },
        "components": [{
            "package-id": FIXTURE_PACKAGE_ID,
            "descriptor": {"component": FIXTURE_COMPONENT, "interface-version": "0.1.0", "component-digest": format!("sha256:{}", "a".repeat(64)), "imports": [], "imports-fingerprint": "", "effects": [], "connections": [],
            "operations": {
                (OPERATION): {"registered-operation": OPERATION, "input-ports": [], "output-ports": [], "parameters": []},
                (EXTRA_OPERATION): {
                    "registered-operation": EXTRA_OPERATION,
                    "input-ports": [], "output-ports": [], "parameters": []
                }
            }}
        }],
        "routes": [{"package-id":FIXTURE_PACKAGE_ID,"component":FIXTURE_COMPONENT,"operation":OPERATION,"type":"get"}],
        "attachments": {(ATTACHMENT_ID): {
            "type":"http","package-id":FIXTURE_PACKAGE_ID,"component":FIXTURE_COMPONENT,"operation":OPERATION,
            "definition-hash":definition_hash,"definition":definition,"auth-policy":{"modes":["pat","session"]},
            "registered-operation":OPERATION
        }}

    });
    let bytes = wamn_digest::canonical_json_bytes(&manifest);
    Ok(Arc::new(LoadedRelease::load_canonical_bytes(
        &bytes,
        "route-authentication-live fixture",
        wamn_engine::release_manifest::ReleaseTarget::new(PROJECT_ENV_KEY, ENVIRONMENT)
            .with_applications(
                [(
                    "inventory".to_owned(),
                    wamn_catalog::ApplicationDefinition {
                        connections: std::collections::BTreeMap::new(),
                        packages: [FIXTURE_PACKAGE_ID.to_owned()].into(),
                    },
                )]
                .into(),
            ),
    )?))
}

/// A project plugin over the login at `url` for `class`.
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

async fn routing(
    identity_reader: sqlx::PgPool,
    postgres: Arc<WamnPostgres>,
    loaded_release: Arc<LoadedRelease>,
) -> anyhow::Result<FlowHttpRouting> {
    Ok(
        FlowHttpRouting::new(Some(loaded_release)).with_authenticator(Arc::new(
            PlatformRouteAuthenticator::default().with_authentication(Arc::new(
                RouteAuthentication::new(identity_reader, postgres, PROJECT),
            )),
        )),
    )
}

async fn invoke(
    routing: &FlowHttpRouting,
    loaded_release: &LoadedRelease,
    authorization: Option<&str>,
    router_admissions: &mut usize,
) -> Result<(), Refusal> {
    let caller = routing
        .authenticate_authorization_for_test(ATTACHMENT_ID, authorization)
        .await
        .map_err(|(status, code)| Refusal::Authentication(status, code))?;
    authorize_attachment_for_test(loaded_release, ATTACHMENT_ID, caller.as_ref())
        .map_err(Refusal::Permission)?;
    *router_admissions += 1;
    Ok(())
}

async fn issue_targeted_token(
    admin: &mut Client,
    project: &str,
    environment: &str,
) -> anyhow::Result<String> {
    let subject = operator_subject(ORG, project, environment)?;
    let principal = create_service(
        admin,
        &subject,
        &format!("operator {project}/{environment}"),
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await
    .context("create wrong-target operator service")?;
    assign_project_role(
        admin,
        principal.id(),
        ORG,
        project,
        ADMIN_ROLE,
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await
    .context("assign wrong-target operator role")?;
    Ok(
        issue_pat(admin, principal.id(), "operator", Duration::from_secs(3600))
            .await
            .context("issue wrong-target route PAT")?
            .token()
            .to_owned(),
    )
}

async fn issue_pat_for_subject(
    admin: &mut Client,
    subject: &str,
    label: &str,
) -> anyhow::Result<(String, String)> {
    let principal = resolve_subject(admin, PrincipalType::Service, subject)
        .await
        .context("resolve the operator principal")?
        .context("the operator principal is absent")?;
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

async fn password_session(
    admin_url: &str,
    principal: &wamn_identity_protocol::PrincipalId,
    email: &str,
    audience: &str,
    roles: &[&str],
) -> anyhow::Result<String> {
    use wamn_platform_identity::{password, password_login};
    let mut client = wamn_postgres::connect(admin_url).await?;

    let actor = wamn_identity_protocol::PlatformComponent::Provisioning
        .principal_id()
        .to_string()
        .parse()?;
    let invitation = password::issue_invitation(&mut client, &actor, principal).await?;
    let work = password::password_work();
    let secret = || password::Password::new("a private membership test password".into());
    password::enroll_password(
        &mut client,
        &work,
        principal,
        invitation.secret(),
        secret()?,
    )
    .await?;
    let mut tx = client.begin().await?;
    assert!(password_login::lock_principal(&mut tx, principal).await?);
    let authenticated = password::authenticate_password(&mut tx, &work, email, secret()?)
        .await?
        .context("password login refused")?;
    let renewal = password_login::create_login(
        &mut tx,
        authenticated.principal().id(),
        session_fixture::ISSUER,
        audience,
    )
    .await?
    .context("active password principal")?;
    let login = renewal.login.id;
    tx.commit().await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    Ok(session_fixture::signed(
        &session_fixture::header(),
        &serde_json::json!({
            "iss": session_fixture::ISSUER, "sub": principal.to_string(), "org": ORG,
            "aud": audience, "roles": roles, "iat": now, "exp": now + 900,
            "authority": {"login": login}, "jti": uuid::Uuid::new_v4().to_string(),
        }),
    ))
}

async fn assert_user_environment_membership(
    admin: &mut Client,
    admin_url: &str,
    identity_url: &str,
    project: &mut Client,
    identity_reader: sqlx::PgPool,
    postgres: Arc<WamnPostgres>,
    loaded_release: Arc<LoadedRelease>,
) -> anyhow::Result<()> {
    let user = create_user(
        admin,
        "member@example.test",
        "member@example.test",
        "Environment member",
    )
    .await?;
    let other = create_user(
        admin,
        "other@example.test",
        "other@example.test",
        "Other member",
    )
    .await?;
    let server = session_fixture::Server::start().await;
    let suffix: String = sqlx::query("SELECT instance_suffix FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3").bind(&ORG).bind(&PROJECT).bind(&ENVIRONMENT).fetch_one(&mut *admin).await?.get(0);
    let audience = wamn_control_provision::session_target::session_audience(
        &wamn_registry::Triple {
            org: ORG.into(),
            project: PROJECT.into(),
            env: ENVIRONMENT.into(),
        },
        &suffix,
    )?;
    let verifier =
        wamn_identity_protocol::verifier::SessionVerifier::new(server.cache().0, ORG, &audience)?;
    let session_routing = FlowHttpRouting::new(Some(Arc::clone(&loaded_release)))
        .with_authenticator(Arc::new(
            PlatformRouteAuthenticator::default().with_session_authentication(Arc::new(
                SessionRouteAuthentication::new(verifier, identity_reader, postgres, PROJECT),
            )),
        ));
    let route_auth = &session_routing;
    let loaded_release = loaded_release.as_ref();
    let authorization = format!(
        "Bearer {}",
        password_session(
            admin_url,
            user.id(),
            "member@example.test",
            &audience,
            &["user-reader", "user-extra"],
        )
        .await?
    );
    let membership = |org: &str, env: &str, principal_id: &str| MembershipRequest {
        org: org.to_owned(),
        project: PROJECT.to_owned(),
        env: env.to_owned(),
        principal_id: principal_id.to_owned(),
        system_database_url: admin_url.to_owned(),
    };
    sqlx::raw_sql("INSERT INTO registry.orgs (id, placement_type, pool_cluster) \
         VALUES ('other-org', 'pooled', 'route-auth-pg18'); \
         INSERT INTO registry.projects (org, id) \
         VALUES ('other-org', 'receiving'), ('acme', 'other'); \
         INSERT INTO registry.env_policies \
           (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) \
         VALUES ('other-org', 'dev', '\"own\"'::jsonb, 1, 1, '1Gi', '1', '1Gi', 'postgres:18'); \
         WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ('acme','prod','prod'),('other-org','dev','dev'),('acme','dev','dev') ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,secret_namespace,instance_suffix) VALUES ('acme','receiving','prod','membership-prod','wamn-system','member01'),('other-org','receiving','dev','membership-other','wamn-system','member02'),('acme','other','dev','membership-project','wamn-system','member03');").execute(&mut *admin).await.map(drop)?;
    sqlx::query(
        "INSERT INTO platform.\"user\" (id, type, email) \
         VALUES ($1::text::uuid, 'user', 'member@example.test'), \
                ($2::text::uuid, 'user', 'other@example.test')",
    )
    .bind(&user.id().to_string().as_str())
    .bind(&other.id().to_string().as_str())
    .execute(&mut *project)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO platform.role (name) VALUES ('user-reader'), ('user-extra')")
        .execute(&mut *project)
        .await
        .map(|result| result.rows_affected())?;
    sqlx::query(
        "INSERT INTO platform.user_role (user_id, role_name) \
         VALUES ($1::text::uuid, 'user-reader'), ($1::text::uuid, 'user-extra')",
    )
    .bind(&user.id().to_string().as_str())
    .execute(&mut *project)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query(
        "INSERT INTO platform.permission (role_name, permission) \
         VALUES ('user-reader', $1), ('user-extra', 'route-auth-fixture:item/extra')",
    )
    .bind(&OPERATION_REFERENCE)
    .execute(&mut *project)
    .await
    .map(|result| result.rows_affected())?;
    // The same principal and role in another environment hold a different grant.
    let other_database = wamn_test_postgres::database();
    let mut other_project = connect(other_database.url()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::sql::ensure_app_acl_role_sql()).to_string(),
    ))
    .execute(&mut other_project)
    .await
    .map(drop)?;
    wamn_dev_control::dev::environment::install_journey_platform_base_grants(
        &mut other_project,
        "example.test",
    )
    .await?;
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:other-environment-fixture', false)",
    )
    .bind(&user.id().to_string().as_str())
    .execute(&mut other_project)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query(
        "INSERT INTO platform.\"user\" (id, type, email) \
             VALUES ($1::text::uuid, 'user', 'member@example.test')",
    )
    .bind(&user.id().to_string().as_str())
    .execute(&mut other_project)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("INSERT INTO platform.role (name) VALUES ('user-reader'); \
             INSERT INTO platform.permission (role_name, permission) \
             VALUES ('user-reader', 'route-auth-fixture:item/other-project')")
            .to_string(),
    ))
    .execute(&mut other_project)
    .await
    .map(drop)?;
    sqlx::query(
        "INSERT INTO platform.user_role (user_id, role_name) \
             VALUES ($1::text::uuid, 'user-reader')",
    )
    .bind(&user.id().to_string().as_str())
    .execute(&mut other_project)
    .await
    .map(|result| result.rows_affected())?;
    drop(other_project);
    // A project-wide role and another environment's membership cannot authorize this route.
    wamn_platform_identity::org::activate_org_membership(admin, user.id(), ORG).await?;
    assign_project_role(
        admin,
        user.id(),
        ORG,
        PROJECT,
        ADMIN_ROLE,
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await?;
    let unauthorized = Refusal::Authentication(401, "unauthorized".to_owned());
    let mut admissions = 0;
    assert_eq!(
        invoke(
            route_auth,
            loaded_release,
            Some(&authorization),
            &mut admissions
        )
        .await,
        Err(unauthorized.clone())
    );
    for (org, env) in [(ORG, OTHER_ENVIRONMENT), ("other-org", ENVIRONMENT)] {
        fixture_membership::grant(membership(org, env, user.id().to_string().as_str())).await?;
        assert_eq!(
            invoke(
                route_auth,
                loaded_release,
                Some(&authorization),
                &mut admissions
            )
            .await,
            Err(unauthorized.clone()),
            "membership in {org}/{PROJECT}/{env} authorized a different environment"
        );
    }
    let mut other_project = membership(ORG, ENVIRONMENT, user.id().to_string().as_str());
    other_project.project = OTHER_PROJECT.to_owned();
    fixture_membership::grant(other_project).await?;
    assert_eq!(
        invoke(
            route_auth,
            loaded_release,
            Some(&authorization),
            &mut admissions
        )
        .await,
        Err(unauthorized.clone()),
        "membership in another project authorized this route"
    );
    let mut forbidden_writer = membership(ORG, ENVIRONMENT, user.id().to_string().as_str());
    forbidden_writer.system_database_url = identity_url.to_owned();
    fixture_membership::grant(forbidden_writer)
        .await
        .expect_err("the identity reader cannot provision membership");
    assert_eq!(
        invoke(
            route_auth,
            loaded_release,
            Some(&authorization),
            &mut admissions
        )
        .await,
        Err(unauthorized.clone())
    );
    assert_eq!(admissions, 0, "a nonmember reached application admission");

    for _ in 0..2 {
        fixture_membership::grant(membership(ORG, ENVIRONMENT, user.id().to_string().as_str()))
            .await?;
    }
    assert_eq!(
        sqlx::query(
            "SELECT count(*) FROM identity.project_env_memberships \
         WHERE principal_id = $1::text::uuid AND org = $2 AND project = $3 AND env = $4"
        )
        .bind(&user.id().to_string().as_str())
        .bind(&ORG)
        .bind(&PROJECT)
        .bind(&ENVIRONMENT)
        .fetch_one(&mut *admin)
        .await?
        .get::<i64, _>(0),
        1
    );
    invoke(
        route_auth,
        loaded_release,
        Some(&authorization),
        &mut admissions,
    )
    .await
    .expect("the exact member reaches application admission");
    let caller = route_auth
        .authenticate_authorization_for_test(ATTACHMENT_ID, Some(&authorization))
        .await
        .expect("authenticate password session")
        .expect("authenticated caller");
    assert_eq!(caller.principal_id(), user.id().to_string().as_str());
    assert!(caller.permits(OPERATION));
    assert!(
        caller.permits(EXTRA_OPERATION),
        "all of this user's environment roles contribute permissions"
    );
    assert!(
        !caller.permits("route-auth-fixture:item/other-project@1.0.0"),
        "another project's role leaked"
    );

    fixture_membership::grant(membership(
        ORG,
        ENVIRONMENT,
        other.id().to_string().as_str(),
    ))
    .await?;
    let other_token =
        password_session(admin_url, other.id(), "other@example.test", &audience, &[]).await?;
    assert_eq!(
        invoke(
            route_auth,
            loaded_release,
            Some(&format!("Bearer {other_token}")),
            &mut admissions
        )
        .await,
        Err(Refusal::Permission(OPERATION.into())),
        "membership alone cannot supply another user's permissions"
    );
    sqlx::query("DELETE FROM platform.user_role WHERE user_id = $1::text::uuid AND role_name = 'user-reader'").bind(&user.id().to_string().as_str()).execute(&mut *project).await.map(|result| result.rows_affected())?;
    assert_eq!(
        invoke(
            route_auth,
            loaded_release,
            Some(&authorization),
            &mut admissions
        )
        .await,
        Err(unauthorized.clone()),
        "role removal must affect the next request"
    );
    sqlx::query("INSERT INTO platform.user_role (user_id, role_name) VALUES ($1::text::uuid, 'user-reader')").bind(&user.id().to_string().as_str()).execute(&mut *project).await.map(|result| result.rows_affected())?;
    sqlx::query("UPDATE platform.\"user\" SET status = 'disabled' WHERE id = $1::text::uuid")
        .bind(&user.id().to_string().as_str())
        .execute(&mut *project)
        .await
        .map(|result| result.rows_affected())?;
    assert_eq!(
        invoke(
            route_auth,
            loaded_release,
            Some(&authorization),
            &mut admissions
        )
        .await,
        Err(unauthorized.clone())
    );
    sqlx::query("UPDATE platform.\"user\" SET status = 'active' WHERE id = $1::text::uuid")
        .bind(&user.id().to_string().as_str())
        .execute(&mut *project)
        .await
        .map(|result| result.rows_affected())?;
    invoke(
        route_auth,
        loaded_release,
        Some(&authorization),
        &mut admissions,
    )
    .await
    .expect("restored environment role authorizes again");
    for _ in 0..2 {
        fixture_membership::revoke(
            membership(ORG, ENVIRONMENT, user.id().to_string().as_str()),
            &Publisher::default(),
        )
        .await?;
        assert_eq!(
            invoke(
                route_auth,
                loaded_release,
                Some(&authorization),
                &mut admissions
            )
            .await,
            Err(unauthorized.clone())
        );
    }
    fixture_membership::grant(membership(ORG, ENVIRONMENT, user.id().to_string().as_str())).await?;
    disable_principal(admin, user.id(), &mut AuthDrops::default()).await?;
    assert_eq!(
        invoke(
            route_auth,
            loaded_release,
            Some(&authorization),
            &mut admissions
        )
        .await,
        Err(unauthorized)
    );
    assert_eq!(
        admissions, 2,
        "a refused user request reached application admission"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_IDENTITY_BINARY"]
async fn production_operator_authentication_and_operation_authorization() {
    use futures_util::FutureExt as _;
    use std::os::unix::fs::DirBuilderExt as _;
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_IDENTITY_BINARY",
    );

    let results = std::env::var_os("TMPDIR")
        .map_or_else(|| PathBuf::from("/var/tmp"), PathBuf::from)
        .join(format!("wamn-route-auth-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&results)
        .expect("create route-auth results");
    let results = results.canonicalize().expect("resolve route-auth results");
    let test_log = results.join("test.log");
    let identity_log = results.join("identity.log");
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::fs::File::create(&test_log).expect("create test log"))
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let scratch = ScratchRoot::create().expect("create route-auth test directory");
    // Keep a second name for the output before the child starts. Scratch cleanup
    // removes only its link, even if the test panics during provisioning.
    std::fs::File::create(&identity_log).expect("create identity log");
    std::os::unix::fs::symlink(&identity_log, scratch.path().join("identity.log"))
        .expect("retain identity output outside the private scratch directory");
    println!(
        "route-auth logs: {}; {}",
        test_log.display(),
        identity_log.display()
    );
    tracing::info!("production operator authentication test started");
    let result = std::panic::AssertUnwindSafe(production_operator_authentication(scratch.path()))
        .catch_unwind()
        .await;
    if let Err(panic) = result {
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("non-text panic");
        tracing::error!(
            failure = message,
            "production operator authentication test failed"
        );
        panic!(
            "{message}\ntest log: {}\nidentity log: {}",
            test_log.display(),
            identity_log.display()
        );
    }
    tracing::info!("production operator authentication test passed");
}

async fn production_operator_authentication(root: &Path) {
    wamn_dev_control::dev::pat_issuer::identity_binary().expect("find the wamn-identity binary");
    // Provisioning changes roles and database grants of the whole server, so
    // the test starts its own.
    let mut server =
        wamn_test_infrastructure::postgres::start(&[]).expect("start the test PostgreSQL server");
    let admin_url = server
        .create_database("wamn_system")
        .expect("create the wamn_system database")
        .url()
        .to_owned();

    let mut admin = connect(&admin_url).await.expect("connect admin");
    let version: i32 = sqlx::query("SHOW server_version_num")
        .fetch_one(&mut admin)
        .await
        .expect("read PostgreSQL version")
        .get::<String, _>(0)
        .parse()
        .expect("parse PostgreSQL version");
    assert!(
        version >= 180_000,
        "the condition requires PostgreSQL 18 or newer"
    );
    reset_and_install_control(&mut admin)
        .await
        .expect("install the control plane");
    let route = provision_route(
        &JourneyTarget::receiving(),
        &admin_url,
        &mut admin,
        root,
        None,
    )
    .await
    .expect("mint the production operator service");
    assert_eq!(
        route.principal_subject,
        operator_subject(ORG, PROJECT, ENVIRONMENT).expect("derive expected route subject")
    );

    let mut project = connect(&route.database_url)
        .await
        .expect("connect project database");
    install_project_and_reconcile(&mut project, &route.database_url)
        .await
        .expect("install and reconcile the route-authentication fixture");
    grant_service_operator(
        &admin_url,
        &mut project,
        &route.database_url,
        &route.principal_subject,
    )
    .await
    .expect("grant the authored role to the operator service");

    let identity_url = wamn_test_infrastructure::credentials::database_login(
        &admin_url,
        Some(&route.database_url),
        ORG,
        PROJECT,
        ENVIRONMENT,
        WorkloadRoleFamily::IdentityReader,
    )
    .await
    .expect("identity reader login");
    let identity_reader = wamn_postgres::pool(&identity_url, 4)
        .await
        .expect("identity reader pool");
    let http_url = wamn_test_infrastructure::credentials::database_login(
        &admin_url,
        Some(&route.database_url),
        ORG,
        PROJECT,
        ENVIRONMENT,
        WorkloadRoleFamily::HttpAdmitter,
    )
    .await
    .expect("HTTP login");
    let loaded_release = load_serving_release().expect("load the canonical serving release");
    let postgres = project_postgres(AuthorityClass::CallableHttp, &http_url)
        .expect("build project-specific callable-HTTP provider");
    let route_auth = routing(
        identity_reader.clone(),
        Arc::clone(&postgres),
        Arc::clone(&loaded_release),
    )
    .await
    .expect("build route authentication");

    assert_user_environment_membership(
        &mut admin,
        &admin_url,
        &identity_url,
        &mut project,
        identity_reader.clone(),
        Arc::clone(&postgres),
        Arc::clone(&loaded_release),
    )
    .await
    .expect("test user environment membership through the production CLI and route authentication");

    let mut router_admissions = 0;
    let valid = format!("Bearer {}", route.token);
    invoke(
        &route_auth,
        &loaded_release,
        Some(&valid),
        &mut router_admissions,
    )
    .await
    .expect("production-minted caller reaches the production router authorization boundary");
    assert_eq!(router_admissions, 1);

    let forged = format!("Bearer {}", flip_last_hex_digit(&route.token));
    let expired = issue_pat_for_subject(&mut admin, &route.principal_subject, "expired")
        .await
        .expect("mint expiring PAT");
    sqlx::query(
        "UPDATE identity.pats SET expires_at = created_at + interval '1 microsecond' \
             WHERE token_prefix = $1",
    )
    .bind(&expired.1)
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())
    .expect("expire PAT in the server clock");
    let revoked = issue_pat_for_subject(&mut admin, &route.principal_subject, "revoked")
        .await
        .expect("mint revocable PAT");
    assert!(
        route_auth
            .authenticate_authorization_for_test(
                ATTACHMENT_ID,
                Some(&format!("Bearer {}", revoked.0))
            )
            .await
            .expect("authenticate service before PAT revocation")
            .is_some()
    );
    revoke_pat(&mut admin, &revoked.1, &mut AuthDrops::default())
        .await
        .expect("revoke PAT");
    sqlx::query("INSERT INTO registry.projects (org, id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(&ORG)
        .bind(&OTHER_PROJECT)
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())
        .expect("seed wrong-project role target");
    let wrong_project = issue_targeted_token(&mut admin, OTHER_PROJECT, ENVIRONMENT)
        .await
        .expect("mint wrong-project PAT");
    let wrong_environment = issue_targeted_token(&mut admin, PROJECT, OTHER_ENVIRONMENT)
        .await
        .expect("mint wrong-environment PAT");
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
    ] {
        assert_eq!(
            invoke(
                &route_auth,
                &loaded_release,
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

    let principal = resolve_subject(&mut admin, PrincipalType::Service, &route.principal_subject)
        .await
        .expect("resolve the operator service")
        .expect("the operator service remains stored");
    sqlx::query(
        "DELETE FROM identity.project_roles WHERE principal_id = $1::text::uuid \
             AND org = $2 AND project = $3 AND role = $4",
    )
    .bind(&principal.id().to_string().as_str())
    .bind(&ORG)
    .bind(&PROJECT)
    .bind(&ADMIN_ROLE)
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())
    .expect("remove the admin project role");
    assert_eq!(
        invoke(
            &route_auth,
            &loaded_release,
            Some(&valid),
            &mut router_admissions
        )
        .await,
        Err(unauthorized.clone()),
        "role removal must refuse an otherwise valid PAT on the next request"
    );
    // `operator` is no longer a platform role, so it admits nothing.
    for (org, project, role) in [
        ("other-org", PROJECT, ADMIN_ROLE),
        (ORG, OTHER_PROJECT, ADMIN_ROLE),
        (ORG, PROJECT, "project-author"),
        (ORG, PROJECT, "operator"),
    ] {
        assign_project_role(
            &mut admin,
            principal.id(),
            org,
            project,
            role,
            &mut wamn_broker::auth_changes::AuthDrops::default(),
        )
        .await
        .expect("assign a role that cannot authorize this route");
        assert_eq!(
            invoke(
                &route_auth,
                &loaded_release,
                Some(&valid),
                &mut router_admissions
            )
            .await,
            Err(unauthorized.clone()),
            "the role {org}/{project}/{role} authorized the wrong route"
        );
    }
    assign_project_role(
        &mut admin,
        principal.id(),
        ORG,
        PROJECT,
        ADMIN_ROLE,
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await
    .expect("restore the admin project role");
    let mut admin_admissions = 0;
    invoke(
        &route_auth,
        &loaded_release,
        Some(&valid),
        &mut admin_admissions,
    )
    .await
    .expect("the admin project role passes the identity condition");
    assert_eq!(admin_admissions, 1);
    disable_principal(&mut admin, principal.id(), &mut AuthDrops::default())
        .await
        .expect("disable the configured service principal");
    assert_eq!(
        invoke(
            &route_auth,
            &loaded_release,
            Some(&valid),
            &mut router_admissions
        )
        .await,
        Err(unauthorized.clone()),
        "a disabled service passed with a valid PAT and project role"
    );
    sqlx::query("UPDATE identity.principals SET status = 'active', disabled_at = NULL WHERE id = $1::text::uuid").bind(&principal.id().to_string().as_str()).execute(&mut admin).await.map(|result| result.rows_affected()).expect("restore the service principal for permission tests");

    // An operator revokes a credential that is IN USE. The case above revokes
    // before the first request, which every read path refuses by arriving at a
    // dead row. This one authenticates first, so a request that never re-reads
    // identity.pats keeps passing. It is the sibling of the permission case
    // below, and the assertion an authorization cache breaks first
    // (wamn-0h0g.17.21).
    let in_use = issue_pat_for_subject(&mut admin, &route.principal_subject, "revoked in use")
        .await
        .expect("mint a PAT to revoke after it authenticates");
    let in_use_authorization = format!("Bearer {}", in_use.0);
    let mut in_use_admissions = 0;
    invoke(
        &route_auth,
        &loaded_release,
        Some(&in_use_authorization),
        &mut in_use_admissions,
    )
    .await
    .expect("a live PAT reaches router admission before its revocation");
    assert_eq!(in_use_admissions, 1);
    assert!(
        postgres.auth_cache().pat(&in_use.0).is_none(),
        "the host without an auth bucket keeps no PAT approval"
    );
    revoke_pat(&mut admin, &in_use.1, &mut AuthDrops::default())
        .await
        .expect("revoke the PAT that just authenticated");
    assert_eq!(
        invoke(
            &route_auth,
            &loaded_release,
            Some(&in_use_authorization),
            &mut in_use_admissions,
        )
        .await,
        Err(unauthorized),
        "a PAT revoked after it authenticated passed the next request"
    );
    assert_eq!(
        in_use_admissions, 1,
        "the revoked PAT reached router admission"
    );

    sqlx::query(
        "DELETE FROM platform.permission \
             WHERE role_name = $1 AND permission = $2",
    )
    .bind(&ITEM_READER)
    .bind(&OPERATION_REFERENCE)
    .execute(&mut project)
    .await
    .map(|result| result.rows_affected())
    .expect("remove the operation grant");
    assert_eq!(
        invoke(
            &route_auth,
            &loaded_release,
            Some(&valid),
            &mut router_admissions,
        )
        .await
        .expect_err("missing permission must refuse"),
        Refusal::Permission(OPERATION.into())
    );
    assert_eq!(
        router_admissions, 1,
        "missing permission reached router admission"
    );

    let permission_backend_unavailable = routing(
        identity_reader.clone(),
        project_postgres(
            AuthorityClass::ExecutorPlatform,
            "postgresql://unused.invalid/unused",
        )
        .expect("build provider missing the callable-HTTP credential"),
        Arc::clone(&loaded_release),
    )
    .await
    .expect("build permission-unavailable routing");
    assert_eq!(
        invoke(
            &permission_backend_unavailable,
            &loaded_release,
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
    assert_eq!(
        invoke(
            &route_auth,
            &loaded_release,
            Some(&valid),
            &mut router_admissions,
        )
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
}
