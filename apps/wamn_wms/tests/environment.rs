//! WMS environment preparation through the existing control library.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_authoring_model::AuthoringScope;
use wamn_catalog::{ComponentPackageScope, PackageCoordinate};
use wamn_control_provision::{WorkloadRoleFamily, sql};
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::author_wiring::{self, AuthorWiringArgs};
use wamn_ctl::bind_connection::{self, BindConnectionArgs, RequirementType};
use wamn_ctl::dev::environment::{
    JourneyCredentials, ProvisionedRoute, connect, read_json, secret_value,
};
use wamn_ctl::enable_cdc_project_env::EnableCdcProjectEnvArgs;
use wamn_ctl::pat_client::PatIssuerArgs;
use wamn_ctl::print_release_env::{self, ReleaseCarrier};
use wamn_ctl::provision_org::{self, TemplateArg};
use wamn_ctl::provision_project_env::{self, ProvisionProjectEnvArgs, WorkloadGenerationArgs};
use wamn_ctl::publish_release::{self, PublishReleaseArgs, ReleaseWiringTarget};
use wamn_ctl::push_component::PushComponentArgs;
use wamn_ctl::push_release_manifest::{self, PushReleaseManifestArgs};
use wamn_ctl::reconcile_package_data_access::ReconcilePackageDataAccessArgs;
use wamn_ctl::reconcile_run_plane::{self, ReconcileRunPlaneArgs};
use wamn_gate_harness::{environment as shared, journey::JourneyDocument};
use wamn_test_infrastructure::declarations::{
    GateInput, gate_document, render_component_declaration,
};

pub const ORG: &str = "acme";
pub const PROJECT: &str = "wms";
pub const ENVIRONMENT: &str = "dev";
pub const TENANT: &str = "wms-route-auth";
pub(super) const SCHEMA: &str = "wms";
const CLUSTER: &str = "route-auth-pg18";
pub(super) const RELEASE_ID: u32 = 1;

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("WMS test crate has an app parent")
        .to_path_buf()
}

fn repository_root() -> PathBuf {
    package_root()
        .parent()
        .and_then(Path::parent)
        .expect("WMS app has a repository parent")
        .to_path_buf()
}

/// Install the existing control schemas in a fresh disposable cluster.
pub async fn install_control(admin_url: &str, system_url: &str) -> anyhow::Result<()> {
    let (admin, task) = connect(admin_url).await?;
    let created = async {
        admin.batch_execute("DO $$ BEGIN \
          IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
            CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; \
          END IF; END $$;").await?;
        admin.batch_execute(&sql::ensure_control_author_acl_role_sql()).await?;
        admin.batch_execute("CREATE DATABASE wamn_system OWNER wamn_system").await
    }.await;
    drop(admin);
    task.abort();
    created.context("create the WMS control database")?;
    let (system, task) = connect(system_url).await?;
    let installed = async {
        system.batch_execute("SET ROLE wamn_system").await?;
        system.batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL).await?;
        system.batch_execute(wamn_control_provision::CONTROL_PORTABLE_STORE_SQL).await?;
        system.batch_execute("RESET ROLE").await?;
        system.batch_execute(sql::revoke_public_connect_floor_sql()).await?;
        system.batch_execute("DO $$ BEGIN EXECUTE format('REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', current_database()); END $$;").await
    }.await;
    drop(system);
    task.abort();
    installed.context("install the WMS control store and PUBLIC privilege limits")
}

/// Provision WMS through the caller's separate PAT service.
/// The caller stops and revokes that service before preparing the project.
pub async fn provision_project(
    inputs: &JourneyDocument,
    work: &Path,
    admin_url: &str,
    pat_issuer: PatIssuerArgs,
) -> anyhow::Result<ProvisionedRoute> {
    provision_org::run(provision_org::provision_org_args(
        ORG.to_owned(),
        TemplateArg::Trials,
        CLUSTER.to_owned(),
        Some(inputs.system_pg_url.clone()),
    ))
    .await?;
    let database_config: tokio_postgres::Config =
        admin_url.parse().context("parse WMS cluster URL")?;
    let host = match database_config.get_hosts() {
        [tokio_postgres::config::Host::Tcp(host)] => host.clone(),
        _ => anyhow::bail!("WMS provisioning requires one TCP database host"),
    };
    let port = database_config.get_ports().first().copied().unwrap_or(5432);
    let args = ProvisionProjectEnvArgs {
        org: Some(ORG.into()),
        project: Some(PROJECT.into()),
        env: Some(ENVIRONMENT.into()),
        tenant: Some(TENANT.into()),
        disposable: false,
        system_database_url: Some(inputs.system_pg_url.clone()),
        cluster: Some(CLUSTER.into()),
        connection_limit: None,
        app_password: Some("unused-legacy-secret".into()),
        app_host: Some(host),
        app_port: port,
        namespace: inputs.host_secret_namespace.clone(),
        secret_namespace: None,
        target_admin_database_url: None,
        workload: WorkloadGenerationArgs::default(),
        emit_database: Some(work.join("database.json")),
        emit_role_sql: Some(work.join("roles.sql")),
        emit_privilege_sql: Some(work.join("privileges.sql")),
        emit_secret: Some(work.join("project-db.json")),
        pat_issuer,
        emit_management_author_pat_secret: Some(work.join("management-author-pat.json")),
        emit_route_caller_pat_secret: Some(inputs.route_caller_secret_output.clone()),
        revoke_pat_prefix: None,
    };
    shared::provision_project(args, admin_url).await
}

/// Apply the emitted WMS database setup, package, and workload credentials.
/// Call this only after the provisioning PAT service has stopped and revoked its role.
pub async fn prepare_project(
    inputs: &JourneyDocument,
    work: &Path,
    admin_url: &str,
    route: &ProvisionedRoute,
) -> anyhow::Result<JourneyCredentials> {
    let (admin, task) = connect(admin_url).await?;
    let result = shared::apply_project_database(
        admin.as_ref(),
        &route.database_url,
        "unused-legacy-secret",
        &work.join("privileges.sql"),
    )
    .await;
    drop(admin);
    task.abort();
    result?;
    let (project, task) = connect(&route.database_url).await?;
    let installed =
        wamn_ctl::dev::environment::install_journey_platform_floor(project.as_ref()).await;
    drop(project);
    task.abort();
    installed?;
    reconcile_run_plane::run(ReconcileRunPlaneArgs {
        system_database_url: inputs.system_pg_url.clone(),
        admin_database_url: route.database_url.clone(),
        org: ORG.into(),
        project: PROJECT.into(),
        tenant: TENANT.into(),
        env: ENVIRONMENT.into(),
        schema: "wamn_run".into(),
        dry_run: false,
    })
    .await?;
    let gate_directory = work.join("gate-credentials");
    fs::create_dir(&gate_directory)
        .context("create the private WMS authoring credential directory")?;
    fs::set_permissions(&gate_directory, fs::Permissions::from_mode(0o700))?;
    let credentials = JourneyCredentials {
        guest_sql: prepare_family(
            inputs,
            route,
            WorkloadRoleFamily::App,
            &inputs.host_secret_directory.join("guest-sql.json"),
            &inputs.host_secret_namespace,
            true,
        )
        .await?,
        executor_platform: prepare_family(
            inputs,
            route,
            WorkloadRoleFamily::ExecutorPlatform,
            &inputs.host_secret_directory.join("executor-platform.json"),
            &inputs.host_secret_namespace,
            true,
        )
        .await?,
        event_materializer: prepare_family(
            inputs,
            route,
            WorkloadRoleFamily::EventMaterializer,
            &inputs.host_secret_directory.join("event-materializer.json"),
            &inputs.host_secret_namespace,
            true,
        )
        .await?,
        http_admitter: prepare_family(
            inputs,
            route,
            WorkloadRoleFamily::HttpAdmitter,
            &inputs.host_secret_directory.join("http-admitter.json"),
            &inputs.host_secret_namespace,
            true,
        )
        .await?,
        identity_reader: prepare_family(
            inputs,
            route,
            WorkloadRoleFamily::IdentityReader,
            &inputs.host_secret_directory.join("identity-reader.json"),
            &inputs.host_secret_namespace,
            false,
        )
        .await?,
        control_author: prepare_family(
            inputs,
            route,
            WorkloadRoleFamily::ControlAuthor,
            &gate_directory.join("control-author.json"),
            "wamn-system",
            false,
        )
        .await?,
        management_admitter: prepare_family(
            inputs,
            route,
            WorkloadRoleFamily::ManagementAdmitter,
            &gate_directory.join("management-admitter.json"),
            "wamn-system",
            true,
        )
        .await?,
    };
    for path in [
        work.join("management-author-pat.json"),
        work.join("project-db.json"),
        gate_directory.join("control-author.json"),
        gate_directory.join("management-admitter.json"),
    ] {
        private_file(&path)?;
    }
    apply_package::run(ApplyPackageArgs {
        package: package_root(),
        database_url: route.database_url.clone(),
        tenant: TENANT.into(),
    })
    .await?;
    shared::reconcile_package_data_access(ReconcilePackageDataAccessArgs {
        packages: vec![package_root()],
        database_url: route.database_url.clone(),
        tenant: TENANT.into(),
    })
    .await?;
    Ok(credentials)
}

fn generation_args(
    inputs: &JourneyDocument,
    family: WorkloadRoleFamily,
    target: Option<&str>,
    secret: &Path,
    namespace: &str,
) -> ProvisionProjectEnvArgs {
    let mut args =
        wamn_ctl::dev::environment::generation_args(family, &inputs.system_pg_url, target, secret);
    args.org = Some(ORG.into());
    args.project = Some(PROJECT.into());
    args.env = Some(ENVIRONMENT.into());
    args.tenant = Some(TENANT.into());
    args.namespace = namespace.to_owned();
    args
}

async fn prepare_family(
    inputs: &JourneyDocument,
    route: &ProvisionedRoute,
    family: WorkloadRoleFamily,
    secret: &Path,
    namespace: &str,
    project_database: bool,
) -> anyhow::Result<String> {
    provision_project_env::run(generation_args(
        inputs,
        family,
        project_database.then_some(route.database_url.as_str()),
        secret,
        namespace,
    ))
    .await?;
    secret_value(secret, "url")
}

fn private_file(path: &Path) -> anyhow::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    ensure!(
        fs::metadata(path)?.permissions().mode() & 0o777 == 0o600,
        "credential file has an unsafe mode: {}",
        path.display()
    );
    Ok(())
}

fn package_coordinate() -> anyhow::Result<PackageCoordinate> {
    let manifest = read_json(&package_root().join("wamn.json"))?;
    PackageCoordinate::new(
        manifest["package"]["id"]
            .as_str()
            .context("WMS manifest names its package")?,
        manifest["package"]["version"]
            .as_str()
            .context("WMS manifest names its version")?,
    )
    .map_err(Into::into)
}

fn component_declaration(
    template: &Path,
    output: &Path,
    package: &PackageCoordinate,
    alias: &str,
) -> anyhow::Result<()> {
    let source =
        fs::read_to_string(template).with_context(|| format!("read {}", template.display()))?;
    let scope =
        ComponentPackageScope::new(TENANT, package.package_id(), package.package_version())?;
    let declaration = render_component_declaration(&source, &scope, alias)?;
    fs::write(output, serde_json::to_vec_pretty(&declaration)?)?;
    Ok(())
}

/// Publish WMS and its label components, author its wirings, then bind and attest.
pub async fn publish(
    inputs: &JourneyDocument,
    route: &ProvisionedRoute,
    credentials: &JourneyCredentials,
    scenario_worker: &Path,
    label_render_wasm: &Path,
    minio_endpoint: &str,
    evidence: &Path,
) -> anyhow::Result<ReleaseCarrier> {
    let package = package_coordinate()?;
    let root = package_root();
    let repository = repository_root();
    let wms_declaration = evidence.join("wms.declaration.json");
    let label_declaration = evidence.join("label-render.declaration.json");
    let blob_declaration = evidence.join("blob-put.declaration.json");
    component_declaration(
        &root.join("publication/components/wms.json.in"),
        &wms_declaration,
        &package,
        "",
    )?;
    component_declaration(
        &repository.join("apps/platform/no-std/label-render/declaration.json.in"),
        &label_declaration,
        &package,
        "",
    )?;
    component_declaration(
        &repository.join("apps/platform/execution/blob-put/declaration.json.in"),
        &blob_declaration,
        &package,
        "labels",
    )?;
    let component_args = |component_bytes, declaration, admitted: &[&str]| PushComponentArgs {
        package: root.clone(),
        component_bytes,
        declaration,
        artifact_base: inputs.component_artifact_base.clone(),
        registry_auth_file: inputs.registry_auth_file.clone(),
        insecure_registry: true,
        admitted_platform_packages: admitted.iter().map(|value| (*value).to_owned()).collect(),
        project_database_url: route.database_url.clone(),
        control_database_url: inputs.system_pg_url.clone(),
    };
    let wms_digest = shared::push_component(component_args(
        inputs.component_directory.join("wms.wasm"),
        wms_declaration,
        &["wamn:node", "wamn:postgres"],
    ))
    .await?;
    let label_digest = shared::push_component(component_args(
        label_render_wasm.to_path_buf(),
        label_declaration,
        &["wamn:node"],
    ))
    .await?;
    let blob_digest = shared::push_component(component_args(
        inputs.component_directory.join("blob_put.wasm"),
        blob_declaration,
        &["wamn:node", "wasmcloud:blobstore"],
    ))
    .await?;
    fs::write(
        evidence.join("component-digests.json"),
        serde_json::to_vec_pretty(
            &json!({"wms":wms_digest,"label-render":label_digest,"blob-put":blob_digest}),
        )?,
    )?;
    let mut wirings = fs::read_dir(root.join("publication/wirings"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    wirings.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    wirings.sort();
    author_wirings(
        inputs,
        route,
        credentials,
        scenario_worker,
        &package,
        &wirings,
        evidence,
    )
    .await?;
    let targets = wirings
        .iter()
        .map(|path| {
            let document = author_wiring::read_wiring_document(path)?;
            Ok(ReleaseWiringTarget {
                package_id: package.package_id().to_string(),
                package_version: package.package_version().to_string(),
                wiring_id: document.wiring_id,
                wiring_version: document.version,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    publish_release::run(PublishReleaseArgs {
        database_url: route.database_url.clone(),
        control_database_url: inputs.system_pg_url.clone(),
        org: ORG.into(),
        project: PROJECT.into(),
        tenant: TENANT.into(),
        effective_release_id: RELEASE_ID,
        environment: ENVIRONMENT.into(),
        verified_publisher_principal: route
            .management_principal_subject
            .clone()
            .context("WMS provisioning returned its management principal")?,
        run_schema: "wamn_run".into(),
        packages: vec![package],
        wirings: targets,
        attachments: vec![root.join("publication/attachments.json")],
        route_host: Some(inputs.route_host.clone()),
        package_manifests: vec![root.join("wamn.json")],
    })
    .await?;
    let definition = evidence.join("labels-store.definition.json");
    fs::write(
        &definition,
        serde_json::to_vec_pretty(
            &json!({"endpoint":minio_endpoint,"container":"labels","prefix":"wms/"}),
        )?,
    )?;
    bind_connection::run(BindConnectionArgs {
        database_url: route.database_url.clone(),
        tenant: TENANT.into(),
        environment: ENVIRONMENT.into(),
        instance_id: "labels-store".into(),
        requirement_type: RequirementType::Blobstore,
        definition,
        credential_handle: "labels-store".into(),
        effective_release_id: RELEASE_ID,
        component_digest: blob_digest,
        store_alias: "labels".into(),
    })
    .await?;
    push_release_manifest::run(PushReleaseManifestArgs {
        database_url: route.database_url.clone(),
        control_database_url: inputs.system_pg_url.clone(),
        org: ORG.into(),
        project: PROJECT.into(),
        tenant: TENANT.into(),
        effective_release_id: RELEASE_ID,
        artifact_base: inputs.release_artifact_base.clone(),
        registry_auth_file: inputs.registry_auth_file.clone(),
        insecure_registry: true,
    })
    .await?;
    print_release_env::lookup_release_carrier(
        &route.database_url,
        TENANT,
        RELEASE_ID,
        &inputs.release_artifact_base,
    )
    .await
}

async fn author_wirings(
    inputs: &JourneyDocument,
    route: &ProvisionedRoute,
    credentials: &JourneyCredentials,
    binary: &Path,
    package: &PackageCoordinate,
    wirings: &[PathBuf],
    evidence: &Path,
) -> anyhow::Result<()> {
    let bind = "127.0.0.1:18080";
    let listener =
        std::net::TcpListener::bind(bind).context("reserve the WMS authoring address")?;
    drop(listener);
    let log = fs::File::create(evidence.join("authoring-gate.log"))?;
    let mut child = tokio::process::Command::new(binary)
        .args(["serve", "--bind", bind])
        .env("WAMN_SYSTEM_URL", &credentials.identity_reader)
        .env("WAMN_CONTROL_AUTHORING_PG_URL", &credentials.control_author)
        .env(
            "WAMN_MANAGEMENT_ADMISSION_PG_URL",
            &credentials.management_admitter,
        )
        .env("WAMN_MANAGEMENT_ORG", ORG)
        .env("WAMN_MANAGEMENT_PROJECT", PROJECT)
        .env("WAMN_MANAGEMENT_ENVIRONMENT", ENVIRONMENT)
        .env("WAMN_MANAGEMENT_TENANT", TENANT)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()?;
    let result = async {
        let client = reqwest::Client::new();
        let mut ready = false;
        for _ in 0..30 {
            ensure!(
                child.try_wait()?.is_none(),
                "the WMS authoring service stopped before readiness"
            );
            if client.get(format!("http://{bind}/")).send().await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        ensure!(
            ready && child.try_wait()?.is_none(),
            "the WMS authoring service did not become ready"
        );
        let token = route
            .management_token
            .as_ref()
            .context("WMS provisioning returned a management PAT")?;
        for path in wirings {
            let document = author_wiring::read_wiring_document(path)?;
            let request = gate_document(
                &GateInput {
                    command_id: format!("gate-{}-{}", package.package_id(), document.wiring_id),
                    package: package.clone(),
                    scope: AuthoringScope {
                        project_id: PROJECT.into(),
                        environment: ENVIRONMENT.into(),
                    },
                },
                &fs::read_to_string(path)?,
            )?;
            let response = client
                .post(format!("http://{bind}/authoring"))
                .bearer_auth(token)
                .json(&request)
                .send()
                .await?
                .error_for_status()?;
            let output: Value = response.json().await?;
            fs::write(
                evidence.join(format!("gate-{}.json", document.wiring_id)),
                serde_json::to_vec_pretty(&output)?,
            )?;
            ensure!(
                output["body"]["outcome"]["status"] == "completed"
                    && output["body"]["outcome"]["value"]["result"]["report-id"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty()),
                "the authoring service refused {}",
                document.wiring_id
            );
            author_wiring::run(AuthorWiringArgs {
                database_url: route.database_url.clone(),
                control_database_url: inputs.system_pg_url.clone(),
                tenant: TENANT.into(),
                package_id: package.package_id().to_string(),
                package_version: package.package_version().to_string(),
                wiring_document: path.clone(),
            })
            .await?;
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if child.try_wait()?.is_none() {
        child.kill().await?;
    }
    child.wait().await?;
    result
}

/// Enable the same CDC and registry-reader credentials as the WMS script.
pub async fn configure_cdc(
    inputs: &JourneyDocument,
    route: &ProvisionedRoute,
    work: &Path,
    replication_password: &str,
) -> anyhow::Result<()> {
    let database_config: tokio_postgres::Config = route.database_url.parse()?;
    let host = match database_config.get_hosts() {
        [tokio_postgres::config::Host::Tcp(host)] => host.clone(),
        _ => anyhow::bail!("WMS CDC requires one TCP database host"),
    };
    let role_path = work.join("cdc-role.sql");
    let cdc_path = work.join("cdc.sql");
    let secret_path = work.join("cdc-reader.json");
    let registry_path = work.join("registry-reader.json");
    let args = EnableCdcProjectEnvArgs {
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENVIRONMENT.into(),
        schema: SCHEMA.into(),
        system_database_url: Some(inputs.system_pg_url.clone()),
        cluster: Some(CLUSTER.into()),
        replication_password: replication_password.into(),
        db_host: Some(host),
        db_port: database_config.get_ports().first().copied().unwrap_or(5432),
        namespace: inputs.host_secret_namespace.clone(),
        secret_namespace: Some(inputs.host_secret_namespace.clone()),
        stream: None,
        emit_role_sql: Some(role_path),
        emit_cdc_sql: Some(cdc_path),
        emit_secret: Some(secret_path.clone()),
    };
    let (project, task) = connect(&route.database_url).await?;
    let result = shared::configure_cdc(args, project.as_ref(), project.as_ref()).await;
    drop(project);
    task.abort();
    result?;
    provision_project_env::run(generation_args(
        inputs,
        WorkloadRoleFamily::RegistryReader,
        None,
        &registry_path,
        &inputs.host_secret_namespace,
    ))
    .await?;
    private_file(&secret_path)?;
    private_file(&registry_path)?;
    Ok(())
}
