//! Production route authentication and shared test-document checks.


use std::collections::{BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tokio_postgres::Client;
use wamn_catalog::{SERVING_MANIFEST_FORMAT_VERSION};
use wamn_control_provision::{SystemReader, WorkloadRoleFamily, parse_system_reader_url};
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::project_env_membership::{self, ProjectEnvMembershipArgs};
use wamn_ctl::provision_project_env;
use wamn_gate_harness::journey::{journey_document_schema_bytes, parse_journey_document};
use wamn_execution_host::{authorize_attachment_for_test};
use wamn_platform_identity::{PrincipalKind, assign_project_role, create_human, create_service, disable_principal, issue_pat, resolve_subject, revoke_pat, route_caller_subject};
use wamn_runtime::plugins::flow_http_routing::{FlowHttpRouting, RouteAuthentication, RouteInFlightLimit};
use wamn_runtime::plugins::wamn_postgres::{AuthorityClass, CredentialProvider, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig};
use wamn_runtime::release_manifest::LoadedRelease;

use wamn_ctl::dev::environment::{ENVIRONMENT, ORG, PROJECT, TENANT, connect, generation_args, provision_route, reset_control_store, secret_value};
use wamn_test_infrastructure::scratch::ScratchRoot;


const URL_ENV: &str = "WAMN_ROUTE_AUTH_PG18_URL";
const OTHER_PROJECT: &str = "other";
const OTHER_ENVIRONMENT: &str = "prod";
const ROUTE_CALLER_ROLE: &str = "route-caller";
const ATTACHMENT_ID: &str = "receiving-purchase-order-get";
const OPERATION: &str = "wamn-receiving:purchase-order/get@1.0.0";
const BASE_COMPONENT: &str = "receiving";
const RESIDUE: &str = "wamn-receiving:obsolete/operation@1.0.0";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Refusal {
    Authentication(u16, String),
    Permission(Box<str>),
}

async fn reset_and_install_control(admin: &Client) -> anyhow::Result<()> {
    reset_control_store(admin).await?;
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
        .context("seed the auth-only test's declared environment policies")?;
    Ok(())
}

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/wamn_receiving")
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
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
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
        include_bytes!("../../../apps/wamn_receiving/wamn.json"),
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
        8,
        "Receiving declares exactly eight operations"
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

fn load_serving_release() -> anyhow::Result<Arc<LoadedRelease>> {
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
            "component": BASE_COMPONENT,
            "interface-version": "0.1.0",
            "digest": format!("sha256:{}", "a".repeat(64)),
            "operations": {
                (OPERATION): {"registered-operation": OPERATION}
            }
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
                "auth-policy": {"modes": ["pat"]},
                "registered-operation": OPERATION
            }
        },
        "registrations": {}
    });
    let bytes = wamn_execution_contract::canonical_json_bytes(&manifest);
    Ok(Arc::new(LoadedRelease::load_canonical_bytes(
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

async fn routing(
    identity_reader: Arc<Client>,
    postgres: Arc<WamnPostgres>,
    loaded_release: Arc<LoadedRelease>,
) -> anyhow::Result<FlowHttpRouting> {
    Ok(
        FlowHttpRouting::new(Some(loaded_release), RouteInFlightLimit::default()).with_authentication(
            Arc::new(
                RouteAuthentication::new(
                    identity_reader,
                    postgres,
                    ORG,
                    PROJECT,
                    route_caller_subject(ORG, PROJECT, ENVIRONMENT)?,
                )
                .await?,
            ),
        ),
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

async fn assert_human_environment_membership(
    admin: &Client,
    admin_url: &str,
    identity_url: &str,
    project: &Client,
    route_auth: &FlowHttpRouting,
    loaded_release: &LoadedRelease,
) -> anyhow::Result<()> {
    let human = create_human(admin, "member@example.test", "Environment member").await?;
    let other = create_human(admin, "other@example.test", "Other member").await?;
    let token = issue_pat(
        admin,
        human.id(),
        "membership test",
        Duration::from_secs(3600),
    )
    .await?;
    let authorization = format!("Bearer {}", token.token());
    let membership = |org: &str, env: &str, principal_id: &str| ProjectEnvMembershipArgs {
        org: org.to_owned(),
        project: PROJECT.to_owned(),
        env: env.to_owned(),
        principal_id: principal_id.to_owned(),
        system_database_url: admin_url.to_owned(),
    };
    admin.batch_execute(
        "INSERT INTO registry.orgs (id, placement_kind, pool_cluster) \
         VALUES ('other-org', 'pooled', 'route-auth-pg18'); \
         INSERT INTO registry.projects (org, id) \
         VALUES ('other-org', 'receiving'), ('acme', 'other'); \
         INSERT INTO registry.env_policies \
           (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) \
         VALUES ('other-org', 'dev', '\"own\"'::jsonb, 1, 1, '1Gi', '1', '1Gi', 'postgres:18'); \
         INSERT INTO registry.project_envs (org, project, env, secret_name, secret_namespace, instance_suffix) \
         VALUES ('acme', 'receiving', 'prod', 'membership-prod', 'wamn-system', 'member01'), \
                ('other-org', 'receiving', 'dev', 'membership-other', 'wamn-system', 'member02'), \
                ('acme', 'other', 'dev', 'membership-project', 'wamn-system', 'member03');"
    ).await?;
    project
        .execute(
            "INSERT INTO app_system.users (tenant_id, id, email) \
         VALUES ($1, $2::text::uuid, 'member@example.test'), \
                ($1, $3::text::uuid, 'other@example.test'), \
                ('other-tenant', $2::text::uuid, 'member@example.test')",
            &[&TENANT, &human.id().as_str(), &other.id().as_str()],
        )
        .await?;
    project
        .execute(
            "INSERT INTO app_system.roles (tenant_id, name) \
         VALUES ($1, 'human-reader'), ($1, 'human-extra'), \
                ('other-tenant', 'human-reader')",
            &[&TENANT],
        )
        .await?;
    project
        .execute(
            "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) \
         VALUES ($1, $2::text::uuid, 'human-reader'), ($1, $2::text::uuid, 'human-extra'), \
                ('other-tenant', $2::text::uuid, 'human-reader')",
            &[&TENANT, &human.id().as_str()],
        )
        .await?;
    project
        .execute(
            "INSERT INTO app_system.permissions (tenant_id, role_name, permission) \
         VALUES ($1, 'human-reader', $2), ($1, 'human-extra', 'extra-operation'), \
                ('other-tenant', 'human-reader', 'other-tenant-operation')",
            &[&TENANT, &OPERATION],
        )
        .await?;
    // A project-wide role and another environment's membership cannot authorize this route.
    assign_project_role(admin, human.id(), ORG, PROJECT, ROUTE_CALLER_ROLE).await?;
    let unauthorized = Refusal::Authentication(401, "unauthorized".to_owned());
    let mut admissions = 0;
    assert_eq!(
        invoke(route_auth, loaded_release, Some(&authorization), &mut admissions).await,
        Err(unauthorized.clone())
    );
    for (org, env) in [(ORG, OTHER_ENVIRONMENT), ("other-org", ENVIRONMENT)] {
        project_env_membership::grant(membership(org, env, human.id().as_str())).await?;
        assert_eq!(
            invoke(route_auth, loaded_release, Some(&authorization), &mut admissions).await,
            Err(unauthorized.clone()),
            "membership in {org}/{PROJECT}/{env} authorized a different environment"
        );
    }
    let mut other_project = membership(ORG, ENVIRONMENT, human.id().as_str());
    other_project.project = OTHER_PROJECT.to_owned();
    project_env_membership::grant(other_project).await?;
    assert_eq!(
        invoke(route_auth, loaded_release, Some(&authorization), &mut admissions).await,
        Err(unauthorized.clone()),
        "membership in another project authorized this route"
    );
    let mut forbidden_writer = membership(ORG, ENVIRONMENT, human.id().as_str());
    forbidden_writer.system_database_url = identity_url.to_owned();
    project_env_membership::grant(forbidden_writer)
        .await
        .expect_err("the identity reader cannot provision membership");
    assert_eq!(
        invoke(route_auth, loaded_release, Some(&authorization), &mut admissions).await,
        Err(unauthorized.clone())
    );
    assert_eq!(admissions, 0, "a nonmember reached application admission");

    for _ in 0..2 {
        project_env_membership::grant(membership(ORG, ENVIRONMENT, human.id().as_str())).await?;
    }
    assert_eq!(
        admin
            .query_one(
                "SELECT count(*) FROM identity.project_env_memberships \
         WHERE principal_id = $1::text::uuid AND org = $2 AND project = $3 AND env = $4",
                &[&human.id().as_str(), &ORG, &PROJECT, &ENVIRONMENT],
            )
            .await?
            .get::<_, i64>(0),
        1
    );
    invoke(route_auth, loaded_release, Some(&authorization), &mut admissions)
        .await
        .expect("the exact member reaches application admission");
    let caller = route_auth
        .authenticate_authorization_for_test(ATTACHMENT_ID, Some(&authorization))
        .await
        .expect("authenticate human PAT")
        .expect("authenticated caller");
    assert_eq!(caller.principal_id(), human.id().as_str());
    assert!(caller.permits(OPERATION));
    assert!(
        caller.permits("extra-operation"),
        "all of this user's environment roles contribute permissions"
    );
    assert!(
        !caller.permits("other-tenant-operation"),
        "another tenant's role leaked"
    );

    let expired = issue_pat(
        admin,
        human.id(),
        "expired member",
        Duration::from_secs(3600),
    )
    .await?;
    admin
        .execute(
            "UPDATE identity.pats SET created_at = now() - interval '2 hours', \
         expires_at = now() - interval '1 hour' WHERE token_prefix = $1",
            &[&expired.record().prefix()],
        )
        .await?;
    let revoked = issue_pat(
        admin,
        human.id(),
        "revoked member",
        Duration::from_secs(3600),
    )
    .await?;
    let revoked_authorization = format!("Bearer {}", revoked.token());
    assert!(
        route_auth
            .authenticate_authorization_for_test(ATTACHMENT_ID, Some(&revoked_authorization))
            .await
            .expect("authenticate human before PAT revocation")
            .is_some()
    );
    revoke_pat(admin, revoked.record().prefix()).await?;
    for (label, invalid) in [
        ("forged human PAT", flip_last_hex_digit(token.token())),
        ("expired human PAT", expired.token().to_owned()),
        ("revoked human PAT", revoked.token().to_owned()),
    ] {
        assert_eq!(
            invoke(
                route_auth,
                loaded_release,
                Some(&format!("Bearer {invalid}")),
                &mut admissions
            )
            .await,
            Err(unauthorized.clone()),
            "{label} passed with valid environment membership"
        );
    }

    project_env_membership::grant(membership(ORG, ENVIRONMENT, other.id().as_str())).await?;
    let other_token =
        issue_pat(admin, other.id(), "other member", Duration::from_secs(3600)).await?;
    assert_eq!(
        invoke(
            route_auth,
            loaded_release,
            Some(&format!("Bearer {}", other_token.token())),
            &mut admissions
        )
        .await,
        Err(Refusal::Permission(OPERATION.into())),
        "membership alone cannot supply another user's permissions"
    );
    project.execute(
        "DELETE FROM app_system.user_roles WHERE tenant_id = $1 AND user_id = $2::text::uuid AND role_name = 'human-reader'",
        &[&TENANT, &human.id().as_str()],
    ).await?;
    assert_eq!(
        invoke(route_auth, loaded_release, Some(&authorization), &mut admissions).await,
        Err(Refusal::Permission(OPERATION.into())),
        "role removal must affect the next request"
    );
    project.execute(
        "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES ($1, $2::text::uuid, 'human-reader')",
        &[&TENANT, &human.id().as_str()],
    ).await?;
    project.execute(
        "UPDATE app_system.users SET status = 'disabled' WHERE tenant_id = $1 AND id = $2::text::uuid",
        &[&TENANT, &human.id().as_str()],
    ).await?;
    assert_eq!(
        invoke(route_auth, loaded_release, Some(&authorization), &mut admissions).await,
        Err(Refusal::Permission(OPERATION.into()))
    );
    project.execute(
        "UPDATE app_system.users SET status = 'active' WHERE tenant_id = $1 AND id = $2::text::uuid",
        &[&TENANT, &human.id().as_str()],
    ).await?;
    invoke(route_auth, loaded_release, Some(&authorization), &mut admissions)
        .await
        .expect("restored environment role authorizes again");
    for _ in 0..2 {
        project_env_membership::revoke(membership(ORG, ENVIRONMENT, human.id().as_str())).await?;
        assert_eq!(
            invoke(route_auth, loaded_release, Some(&authorization), &mut admissions).await,
            Err(unauthorized.clone())
        );
    }
    project_env_membership::grant(membership(ORG, ENVIRONMENT, human.id().as_str())).await?;
    disable_principal(admin, human.id()).await?;
    assert_eq!(
        invoke(route_auth, loaded_release, Some(&authorization), &mut admissions).await,
        Err(unauthorized)
    );
    assert_eq!(
        admissions, 2,
        "a refused human request reached application admission"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires a fresh disposable PG18 named by WAMN_ROUTE_AUTH_PG18_URL"]
async fn production_route_caller_authentication_and_operation_authorization() {
    let admin_url = std::env::var(URL_ENV)
        .expect("WAMN_ROUTE_AUTH_PG18_URL must name a fresh disposable PostgreSQL 18 server");
    let scratch = ScratchRoot::create().expect("create route-auth test directory");
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
    let route = provision_route(&admin_url, &admin, &root, None)
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
    let loaded_release = load_serving_release().expect("load the canonical serving release");
    let route_auth = routing(
        Arc::clone(&identity_reader),
        project_postgres(AuthorityClass::CallableHttp, &http_url)
            .expect("build project-specific callable-HTTP provider"),
        Arc::clone(&loaded_release),
    )
    .await
    .expect("build route authentication");

    assert_human_environment_membership(
        &admin,
        &admin_url,
        &identity_url,
        &project,
        &route_auth,
        &loaded_release,
    )
    .await
    .expect(
        "test human environment membership through the production CLI and route authentication",
    );

    let mut router_admissions = 0;
    let valid = format!("Bearer {}", route.token);
    invoke(&route_auth, &loaded_release, Some(&valid), &mut router_admissions)
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

    let principal = resolve_subject(
        admin.as_ref(),
        PrincipalKind::Service,
        &route.principal_subject,
    )
    .await
    .expect("resolve route caller")
    .expect("route caller remains stored");
    admin
        .execute(
            "DELETE FROM identity.project_roles WHERE principal_id = $1::text::uuid \
             AND org = $2 AND project = $3 AND role = $4",
            &[&principal.id().as_str(), &ORG, &PROJECT, &ROUTE_CALLER_ROLE],
        )
        .await
        .expect("remove the route-caller role");
    assert_eq!(
        invoke(&route_auth, &loaded_release, Some(&valid), &mut router_admissions).await,
        Err(unauthorized.clone()),
        "role removal must refuse an otherwise valid PAT on the next request"
    );
    for (org, project, role) in [
        ("other-org", PROJECT, ROUTE_CALLER_ROLE),
        (ORG, OTHER_PROJECT, ROUTE_CALLER_ROLE),
        (ORG, PROJECT, "project-author"),
    ] {
        assign_project_role(admin.as_ref(), principal.id(), org, project, role)
            .await
            .expect("assign a role that cannot authorize this route");
        assert_eq!(
            invoke(&route_auth, &loaded_release, Some(&valid), &mut router_admissions).await,
            Err(unauthorized.clone()),
            "the role {org}/{project}/{role} authorized the wrong route"
        );
    }
    assign_project_role(
        admin.as_ref(),
        principal.id(),
        ORG,
        PROJECT,
        ROUTE_CALLER_ROLE,
    )
    .await
    .expect("restore route-caller role");
    disable_principal(admin.as_ref(), principal.id())
        .await
        .expect("disable the configured service principal");
    assert_eq!(
        invoke(&route_auth, &loaded_release, Some(&valid), &mut router_admissions).await,
        Err(unauthorized),
        "a disabled service passed with a valid PAT and project role"
    );
    admin.execute(
        "UPDATE identity.principals SET status = 'active', disabled_at = NULL WHERE id = $1::text::uuid",
        &[&principal.id().as_str()],
    ).await.expect("restore the service principal for permission tests");
    project
        .execute(
            "DELETE FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 AND permission = $3",
            &[&TENANT, &ROUTE_CALLER_ROLE, &OPERATION],
        )
        .await
        .expect("remove the exact operation grant");
    assert_eq!(
        invoke(&route_auth, &loaded_release, Some(&valid), &mut router_admissions,)
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

    identity_task.abort();
    let _ = identity_task.await;
    assert_eq!(
        invoke(&route_auth, &loaded_release, Some(&valid), &mut router_admissions,)
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
/// Checked-in schema generated from [`JourneyDocument`], relative to this
/// crate's manifest. Regenerate with the ignored test beside its drift test.
const JOURNEY_SCHEMA_PATH: &str = "schema/wamn-journey.schema.json";
/// A complete example the shell writer reproduces byte-for-byte and this crate
/// parses, so the two sides are pinned to one artifact rather than to each
/// other's reading of the schema.
const JOURNEY_EXAMPLE_PATH: &str = "schema/wamn-journey.example.json";

#[test]
fn checked_in_journey_schema_matches_generated_bytes() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(JOURNEY_SCHEMA_PATH);
    let checked_in = std::fs::read(&path).expect("read the checked-in journey schema");
    assert_eq!(checked_in, journey_document_schema_bytes());
}

#[test]
#[ignore = "schema regeneration command only"]
fn regenerate_checked_in_journey_schema() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(JOURNEY_SCHEMA_PATH);
    std::fs::write(path, journey_document_schema_bytes())
        .expect("write the generated journey schema");
}

/// The generated schema and the strict parser share ONE field authority: a
/// field cannot exist in the parser and be absent from the schema, or the
/// reverse, and the schema says which fields a writer must supply.
#[test]
fn generated_journey_schema_and_strict_parser_share_one_field_authority() {
    let first = journey_document_schema_bytes();
    assert_eq!(first, journey_document_schema_bytes());
    assert_eq!(first.last(), Some(&b'\n'));

    let schema: serde_json::Value = serde_json::from_slice(&first).expect("parse the schema");
    assert_eq!(schema["additionalProperties"], false);
    let properties = schema["properties"].as_object().expect("object properties");
    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("required set")
        .iter()
        .map(|key| key.as_str().expect("required key"))
        .collect();
    // Every scalar the parser refuses-when-empty is a required property, and
    // the only property that is not required is the phase the shell amends in.
    let example = parse_journey_document(&example_document()).expect("example parses");
    for (field, _) in example.scalars() {
        assert!(properties.contains_key(field), "schema lacks {field}");
        assert!(required.contains(&field), "schema does not require {field}");
    }
    assert_eq!(properties.len(), example.scalars().len() + 5);
    for phase in [
        "materializer",
        "runtime",
        "fresh_only_packages",
        "overlay_compatibility",
        "postcommit",
    ] {
        assert!(properties.contains_key(phase));
        assert!(!required.contains(&phase));
    }
    let runtime = &schema["definitions"]["RuntimePhase"];
    assert_eq!(runtime["additionalProperties"], false);
    assert_eq!(
        runtime["required"],
        serde_json::json!(["pallet_id", "route_endpoint", "to_location_id"])
    );
    let phase = &schema["definitions"]["MaterializerPhase"];
    assert_eq!(phase["additionalProperties"], false);
    assert_eq!(
        phase["required"],
        serde_json::json!(["nats_url", "project_pg_url", "receipt_id"])
    );

    // And the refusals name the field, so a forty-minute run does not fail
    // under a message about something else.
    let mut missing: serde_json::Value =
        serde_json::from_slice(&example_document()).expect("example is JSON");
    missing
        .as_object_mut()
        .expect("object")
        .remove("route_host");
    let error = parse_journey_document(&serde_json::to_vec(&missing).expect("serialize"))
        .expect_err("a missing required field is refused");
    assert!(format!("{error:#}").contains("route_host"), "{error:#}");

    let mut unknown: serde_json::Value =
        serde_json::from_slice(&example_document()).expect("example is JSON");
    unknown["route_hots"] = serde_json::json!("typo.localhost");
    let error = parse_journey_document(&serde_json::to_vec(&unknown).expect("serialize"))
        .expect_err("an unknown field is refused, not ignored");
    assert!(format!("{error:#}").contains("route_hots"), "{error:#}");

    let mut empty: serde_json::Value =
        serde_json::from_slice(&example_document()).expect("example is JSON");
    empty["host_secret_namespace"] = serde_json::json!("");
    let error = parse_journey_document(&serde_json::to_vec(&empty).expect("serialize"))
        .expect_err("an empty value is refused, not passed through");
    assert!(
        format!("{error:#}").contains("host_secret_namespace"),
        "{error:#}"
    );

    let mut no_phase: serde_json::Value =
        serde_json::from_slice(&example_document()).expect("example is JSON");
    no_phase
        .as_object_mut()
        .expect("object")
        .remove("materializer");
    let parsed = parse_journey_document(&serde_json::to_vec(&no_phase).expect("serialize"))
        .expect("the materializer phase is optional until the shell amends it in");
    assert!(parsed.materializer.is_none());
}

/// The checked-in example is what the shell writer reproduces byte-for-byte;
/// parsing it here is what pins the two sides to one artifact.
#[test]
fn the_checked_in_example_document_parses_with_every_field() {
    let document = parse_journey_document(&example_document()).expect("example parses");
    assert_eq!(document.route_host, "example.localhost");
    assert_eq!(document.host_secret_namespace, "wamn-example-journey");
    assert_eq!(
        document.registry_auth_file,
        Path::new("/tmp/example/docker/config.json")
    );
    let phase = document
        .materializer
        .expect("the example carries the amended phase");
    assert_eq!(phase.receipt_id, "00000000-0000-0000-0000-00000000c0de");
    let runtime = document
        .runtime
        .expect("the example carries the runtime phase");
    assert_eq!(runtime.route_endpoint, "http://10.0.0.2:30999");
    assert_eq!(runtime.pallet_id, "00000000-0000-0000-0000-000000000301");
}

fn example_document() -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(JOURNEY_EXAMPLE_PATH))
        .expect("read the checked-in journey example")
}
