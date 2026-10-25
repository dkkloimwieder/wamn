//! WMS environment preparation through the existing control library.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;
use wamn_platform_identity::auth_changes::AuthChanges;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_authoring_model::AuthoringScope;
use wamn_catalog::{ComponentPackageScope, PackageCoordinate, RequirementType};
use wamn_control::apply_package::{self, ApplyPackageRequest};
use wamn_control::author_wiring;
use wamn_control::bind_connection::{self, BindConnectionRequest};
use wamn_control::component_admission::AdmitComponentRequest;
use wamn_control::component_declaration::declared_platform_packages;
use wamn_control::delivery::{Candidate, ReleaseIdentity};
use wamn_control::dev::environment::{JourneyCredentials, connect};
use wamn_control::enable_cdc_project_env::EnableCdcProjectEnvRequest;
use wamn_control::environment::analyse::Installed;
use wamn_control::environment::document::{DeclaredRelease, EnvironmentDocument};
use wamn_control::environment::{Platform, stage};
use wamn_control::package_artifact::{PackageRegistry, PushPackageRequest, push_package};
use wamn_control::pat_client::PatIssuerConfig;
use wamn_control::provision_org::{self, OrgRecord, ProvisionOrgRequest};
use wamn_control::provision_project_env::{
    self, ProvisionProjectEnvRequest, ProvisionedRoute, WorkloadActionRequest, read_json,
    secret_value,
};
use wamn_control::publish_release::{self, PublishReleaseRequest, ReleaseWiringTarget};
use wamn_control::reconcile_package_data_access::ReconcilePackageDataAccessRequest;
use wamn_control::reconcile_run_plane::{self, ReconcileRunPlaneRequest};
use wamn_control::release_carrier::ReleaseCarrier;
use wamn_control_provision::{WorkloadRoleFamily, sql};
use wamn_gate_harness::{environment as shared, journey::JourneyDocument};
use wamn_schema_generator::route_schema;
use wamn_test_infrastructure::declarations::{
    GateInput, gate_document, render_component_declaration,
};

pub(super) const SCHEMA: &str = "wms";
pub(crate) const CLUSTER: &str = "route-auth-pg18";
/// Platform rows take the email `<component>@example.invalid` in this test.
const PLATFORM_DOMAIN: &str = "example.invalid";

/// The release this fixture provisions and publishes: the default WMS
/// journey release, with the packages of a supplied candidate's manifest.
pub(crate) fn identity() -> &'static ReleaseIdentity {
    static IDENTITY: LazyLock<ReleaseIdentity> = LazyLock::new(|| {
        // A cluster case takes its packages from its candidate; the other
        // WMS tests run this checkout's package.
        let candidate = std::env::var_os(wamn_control::delivery::CANDIDATE_ENV)
            .map(|_| Candidate::from_env().expect("read the supplied WMS candidate"));
        ReleaseIdentity {
            org: "acme".to_owned(),
            project: "wms".to_owned(),
            environment: "dev".to_owned(),
            tenant: "acme--wms--dev".to_owned(),
            route_host: "wms.localhost".to_owned(),
            packages: match candidate {
                Some(candidate) => {
                    candidate
                        .manifest()
                        .expect("read the manifest of the supplied WMS candidate")
                        .0
                        .release
                        .packages
                }
                None => {
                    BTreeSet::from([package_coordinate().expect("read the WMS package coordinate")])
                }
            },
        }
    });
    &IDENTITY
}

/// The object-store credential Secret of the released environment, which the
/// host overlay mounts and the cluster fixture writes.
pub(crate) fn object_store_secret_name() -> String {
    format!(
        "wamn-object-store-credentials-{}--{}--{}",
        identity().org,
        identity().project,
        identity().environment,
    )
}

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("WMS test crate has an app parent")
        .to_path_buf()
}

/// The package artifact registry, `<registry>/<org>/packages` beside the
/// component artifact base.
fn package_registry(inputs: &JourneyDocument) -> anyhow::Result<PackageRegistry> {
    let (artifact_parent, _) = inputs
        .component_artifact_base
        .rsplit_once('/')
        .context("the component artifact base names a repository")?;
    Ok(PackageRegistry {
        artifact_base: format!("{artifact_parent}/packages"),
        registry_auth_file: inputs.registry_auth_file.clone(),
        insecure_registry: true,
        oci_ca_paths: Vec::new(),
    })
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
        admin.batch_execute(sql::ensure_db_owner_role_sql()).await?;
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
        system.execute(
            "UPDATE registry.meta SET platform_domain = $1",
            &[&PLATFORM_DOMAIN],
        ).await?;
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
    pat_issuer: PatIssuerConfig,
) -> anyhow::Result<ProvisionedRoute> {
    provision_org::provision_org(ProvisionOrgRequest {
        org: identity().org.clone(),
        template: wamn_control_registry::Template::trials(),
        pool: CLUSTER.to_owned(),
        cluster_namespace: "wamn-system".into(),
        record: Some(OrgRecord {
            system_database_url: inputs.system_pg_url.clone(),
            admin: "org-admin@example.invalid".into(),
        }),
    })
    .await?;
    let args = ProvisionProjectEnvRequest {
        changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
        org: identity().org.clone(),
        project: identity().project.clone(),
        env: identity().environment.clone(),
        policy: None,
        disposable: None,
        system_database_url: Some(inputs.system_pg_url.clone()),
        cluster: Some(CLUSTER.into()),
        connection_limit: None,
        cluster_namespace: "wamn-system".into(),
        namespace: inputs.host_secret_namespace.clone(),
        secret_namespace: None,
        emit_database: Some(work.join("database.json")),
        emit_role_sql: Some(work.join("roles.sql")),
        emit_privilege_sql: Some(work.join("privileges.sql")),
        emit_secret: Some(work.join("project-db.json")),
        pat_issuer,
        emit_management_author_pat_secret: Some(work.join("management-author-pat.json")),
        emit_operator_pat_secret: Some(inputs.operator_secret_output.clone()),
    };
    shared::provision_project(args, admin_url).await
}

/// Apply the emitted WMS database setup and workload credentials.
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
        &work.join("privileges.sql"),
    )
    .await;
    drop(admin);
    task.abort();
    result?;
    let (project, task) = connect(&route.database_url).await?;
    let installed = wamn_control::dev::environment::install_journey_platform_floor(
        project.as_ref(),
        PLATFORM_DOMAIN,
    )
    .await;
    drop(project);
    task.abort();
    installed?;
    reconcile_run_plane::reconcile_run_plane(ReconcileRunPlaneRequest {
        changes: AuthChanges::default(),
        system_database_url: inputs.system_pg_url.clone(),
        admin_database_url: route.database_url.clone(),
        org: identity().org.clone(),
        project: identity().project.clone(),
        tenant: identity().tenant.clone(),
        env: identity().environment.clone(),
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
        administration: prepare_family(
            inputs,
            route,
            WorkloadRoleFamily::Administration,
            &inputs.host_secret_directory.join("administration.json"),
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
    Ok(credentials)
}

/// The operator's admin role and the replica identity of the package at
/// `package`, after its installation. The label workflow's condition reads
/// the old packaging row, so the operator's replica identity verb runs the
/// table with REPLICA IDENTITY FULL.
async fn after_install(route: &ProvisionedRoute, package: PathBuf) -> anyhow::Result<()> {
    wamn_control::dev::environment::grant_operator_admin_role(
        &route.database_url,
        identity().org.as_str(),
        identity().project.as_str(),
        identity().environment.as_str(),
        identity().tenant.as_str(),
        PLATFORM_DOMAIN,
    )
    .await?;
    wamn_control::reconcile_replica_identity::reconcile_package_replica_identity(
        wamn_control::reconcile_replica_identity::ReconcileReplicaIdentityRequest {
            admin_database_url: route.database_url.clone(),
            package,
            dry_run: false,
        },
    )
    .await
    .map(drop)
}

/// Install the candidate's release in the provisioned environment as `apply`
/// does, and publish nothing (docs/plan/platform-deploy.md §8.2, §10.2): its
/// packages through the package stage from their artifacts, the operator's
/// admin role and the replica identity of the WMS package, and a binding of
/// each connection the release requires to the labels store at
/// `minio_endpoint`, named after its store alias.
pub async fn install(
    candidate: &Candidate,
    inputs: &JourneyDocument,
    route: &ProvisionedRoute,
    packages: &[wamn_control::package_artifact::OpenedPackage],
    minio_endpoint: &str,
) -> anyhow::Result<()> {
    wamn_gate_harness::case_release::install_candidate(
        candidate,
        &inputs.system_pg_url,
        &route.database_url,
        &wamn_control_registry::Triple::new(
            identity().org.as_str(),
            identity().project.as_str(),
            identity().environment.as_str(),
        ),
    )
    .await?;
    for package in packages {
        after_install(route, package.root().to_path_buf()).await?;
    }
    let (manifest, digest) = candidate.manifest()?;
    for component in &manifest.components {
        for connection in &component.descriptor.connections {
            ensure!(
                connection.requirement_type == wamn_catalog::ComponentConnectionType::Blobstore,
                "the WMS case binds only the labels store, not {}",
                connection.store_alias
            );
            bind_connection::bind(&BindConnectionRequest {
                database_url: route.database_url.clone(),
                tenant: identity().tenant.clone(),
                environment: identity().environment.clone(),
                instance_id: connection.store_alias.clone(),
                requirement_type: RequirementType::Blobstore,
                definition: json!({"endpoint":minio_endpoint,"container":"labels","prefix":"wms/"}),
                credential_handle: Some(connection.store_alias.clone()),
                org_connection_name: None,
                manifest_digest: digest.as_str().to_owned(),
                component_digest: component.descriptor.component_digest.as_str().to_owned(),
                store_alias: connection.store_alias.clone(),
            })
            .await?;
        }
    }
    Ok(())
}

fn generation_args(
    inputs: &JourneyDocument,
    family: WorkloadRoleFamily,
    target: Option<&str>,
    secret: &Path,
    namespace: &str,
) -> WorkloadActionRequest {
    let mut args = wamn_control::dev::environment::generation_args(
        &identity().into(),
        family,
        &inputs.system_pg_url,
        target,
        secret,
    );
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
    provision_project_env::run_workload_action(&generation_args(
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

pub(crate) fn package_coordinate() -> anyhow::Result<PackageCoordinate> {
    let manifest = read_json(&wamn_schema_generator::package_manifest_path(
        &package_root(),
    ))?;
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
) -> anyhow::Result<Vec<String>> {
    let mut source: Value = serde_json::from_slice(
        &fs::read(template).with_context(|| format!("read {}", template.display()))?,
    )?;
    // A package template sits at `publication/components/`, and generation
    // writes the entries of the package's generated operations beside it.
    let package_root = template
        .ancestors()
        .nth(3)
        .context("a declaration template sits under a package root")?;
    let generated = route_schema::read_generated_publication(
        package_root,
        route_schema::GENERATED_COMPONENT_OPERATIONS,
    )?;
    route_schema::merge_operations(&mut source, generated.as_ref())?;
    let scope = ComponentPackageScope::new(
        identity().tenant.as_str(),
        package.package_id(),
        package.package_version(),
    )?;
    let declaration = render_component_declaration(&source.to_string(), &scope, alias)?;
    fs::write(output, serde_json::to_vec_pretty(&declaration)?)?;
    // The grant is the platform packages the declaration states, as the dev
    // loop admits a palette component.
    Ok(declared_platform_packages(
        template,
        &serde_json::to_value(&declaration)?,
    )?)
}

/// Render the WMS template as `push-package` does: the version comes from
/// the package's `wamn.json`, and the template authors none (wamn-uqs5.3).
fn wms_component_declaration(root: &Path, tenant: &str, output: &Path) -> anyhow::Result<()> {
    let declaration = wamn_control::component_declaration::render_declaration_document(
        &root.join("publication/components/wms.json.in"),
        tenant,
    )?;
    fs::write(output, serde_json::to_vec_pretty(&declaration)?)?;
    Ok(())
}

#[test]
fn the_wms_declaration_takes_its_version_from_the_package() -> anyhow::Result<()> {
    let output = std::env::temp_dir().join(format!("wms-declaration-{}.json", std::process::id()));
    let rendered = wms_component_declaration(&package_root(), "wms", &output)
        .and_then(|()| Ok(serde_json::from_slice::<Value>(&fs::read(&output)?)?));
    let _ = fs::remove_file(&output);
    let scope = &rendered?["scope"];
    assert_eq!(scope["tenant-id"], "wms");
    assert_eq!(scope["package-id"], "wamn_wms");
    assert_eq!(
        scope["package-version"].as_str(),
        Some(package_coordinate()?.package_version())
    );
    Ok(())
}

/// The store alias the label wiring's blob-put node names: the one authored
/// alias, which the declaration and the binding both take.
fn label_store_alias(root: &Path) -> anyhow::Result<String> {
    let path = root.join("publication/wirings/inventory_move_and_label.json");
    let wiring: Value = serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )?;
    wiring
        .pointer("/nodes/store/params/store_alias")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("{} names no store alias on its store node", path.display()))
}

/// Publish WMS and its label components, author its wirings, then bind.
/// The artifacts and endpoint one release publication is published from.
pub struct PublicationArtifacts<'a> {
    pub scenario_worker: &'a Path,
    pub label_render_wasm: &'a Path,
    pub minio_endpoint: &'a str,
}

/// `platform` is where the package stage of the release reads its package
/// artifacts, over TLS.
pub async fn publish(
    inputs: &JourneyDocument,
    route: &ProvisionedRoute,
    credentials: &JourneyCredentials,
    artifacts: &PublicationArtifacts<'_>,
    platform: &Platform,
    evidence: &Path,
) -> anyhow::Result<ReleaseCarrier> {
    let PublicationArtifacts {
        scenario_worker,
        label_render_wasm,
        minio_endpoint,
    } = *artifacts;
    // The package is applied as the artifact it is pushed as, so publish
    // reads one digest for it (docs/plan/platform-deploy.md R3).
    let pushed = push_package(&PushPackageRequest {
        package: package_root(),
        registry: package_registry(inputs)?,
        component_artifact_base: inputs.component_artifact_base.clone(),
    })
    .await
    .context("push the WMS package artifact")?;
    let installed = wamn_control::apply_package::PackageRoot {
        path: package_root(),
        artifact_digest: Some(pushed.digest),
    };
    apply_package::apply_package(ApplyPackageRequest {
        changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
        package: installed.clone(),
        database_url: route.database_url.clone(),
        tenant: identity().tenant.clone(),
    })
    .await?;
    shared::reconcile_package_data_access(ReconcilePackageDataAccessRequest {
        packages: vec![installed],
        database_url: route.database_url.clone(),
        tenant: identity().tenant.clone(),
    })
    .await?;
    after_install(route, package_root()).await?;
    let package = package_coordinate()?;
    let root = package_root();
    let repository = repository_root();
    let wms_declaration = evidence.join("wms.declaration.json");
    let label_declaration = evidence.join("label-render.declaration.json");
    let blob_declaration = evidence.join("blob-put.declaration.json");
    let jsonata_declaration = evidence.join("jsonata.declaration.json");
    let store_alias = label_store_alias(&root)?;
    wms_component_declaration(&root, identity().tenant.as_str(), &wms_declaration)?;
    let label_packages = component_declaration(
        &repository.join("apps/platform/no-std/label-render/declaration.json.in"),
        &label_declaration,
        &package,
        "",
    )?;
    let blob_packages = component_declaration(
        &repository.join("apps/platform/execution/blob-put/declaration.json.in"),
        &blob_declaration,
        &package,
        &store_alias,
    )?;
    let jsonata_packages = component_declaration(
        &repository.join("apps/platform/execution/jsonata/declaration.json.in"),
        &jsonata_declaration,
        &package,
        "",
    )?;
    let admit_request = |component_bytes, declaration, admitted| AdmitComponentRequest {
        package: root.clone(),
        component_bytes,
        declaration,
        admitted_platform_packages: admitted,
        base_digests: std::collections::BTreeSet::new(),
    };
    let wms_digest = shared::admit_component_digest(admit_request(
        inputs.component_directory.join("wms.wasm"),
        wms_declaration,
        vec!["wamn:node".to_owned(), "wamn:postgres".to_owned()],
    ))?;
    let label_digest = shared::admit_component_digest(admit_request(
        label_render_wasm.to_path_buf(),
        label_declaration,
        label_packages,
    ))?;
    let blob_digest = shared::admit_component_digest(admit_request(
        inputs.component_directory.join("blob_put.wasm"),
        blob_declaration,
        blob_packages,
    ))?;
    let jsonata_digest = shared::admit_component_digest(admit_request(
        inputs.component_directory.join("jsonata_expression.wasm"),
        jsonata_declaration,
        jsonata_packages,
    ))?;
    fs::write(
        evidence.join("component-digests.json"),
        serde_json::to_vec_pretty(&json!({"wms":wms_digest,"label-render":label_digest,
            "blob-put":blob_digest,"jsonata":jsonata_digest}))?,
    )?;
    // The gate, authorship and the release read the component facts from
    // the verified package artifact that `prepare_project` pushed.
    let registry = package_registry(inputs)?;
    let mut wirings = fs::read_dir(root.join("publication/wirings"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    wirings.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    wirings.sort();
    author_wirings(
        route,
        credentials,
        scenario_worker,
        &package,
        &registry,
        &inputs.system_pg_url,
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
    let release_digest = publish_release::publish_release(
        PublishReleaseRequest {
            database_url: route.database_url.clone(),
            control_database_url: inputs.system_pg_url.clone(),
            org: identity().org.clone(),
            tenant: identity().tenant.clone(),
            environment: identity().environment.clone(),
            verified_publisher_principal: route
                .management_principal_subject
                .clone()
                .context("WMS provisioning returned its management principal")?,
            run_schema: "wamn_run".into(),
            packages: vec![package.clone()],
            wirings: targets,
        },
        &registry,
        &inputs.release_artifact_base,
    )
    .await?
    .to_string();
    // Publish writes OCI only; the package stage below writes the row.
    let bytes = wamn_runtime::release_manifest_source::ReleaseManifestSource::new(
        &inputs.release_artifact_base,
        true,
        &inputs.registry_auth_file,
    )?
    .pull_verified(&release_digest)
    .await
    .context("pull the exact WMS release")?;
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)?;
    // A binding names the connection requirements that the package stage
    // projects, so the release's stage runs first, on the package installed
    // above (docs/plan/platform-deploy.md §10.2). The
    // instance and its credential handle are named after the store alias, as
    // `env apply` names the instances it binds.
    let version = package.package_version().to_owned();
    stage::stage(
        platform,
        &EnvironmentDocument {
            org: identity().org.clone(),
            project: identity().project.clone(),
            env: identity().environment.clone(),
            release: DeclaredRelease::Digest(release_digest.clone()),
            route_host: identity().route_host.clone(),
            client_package: None,
            policy: String::new(),
            applications: BTreeMap::from([(
                package.package_id().to_owned(),
                wamn_catalog::ApplicationDefinition {
                    packages: BTreeSet::from([package.package_id().to_owned()]),
                },
            )]),
            connections: BTreeMap::new(),
            floors: BTreeMap::new(),
        },
        &BTreeMap::from([(
            package.package_id().to_owned(),
            Installed {
                current: version.clone(),
                lineage: BTreeMap::from([(version, None)]),
                evidence: BTreeSet::new(),
            },
        )]),
        &release_digest,
        &manifest,
        &route.database_url,
    )
    .await?;
    let definition = json!({"endpoint":minio_endpoint,"container":"labels","prefix":"wms/"});
    bind_connection::bind(&BindConnectionRequest {
        database_url: route.database_url.clone(),
        tenant: identity().tenant.clone(),
        environment: identity().environment.clone(),
        instance_id: store_alias.clone(),
        requirement_type: RequirementType::Blobstore,
        definition,
        credential_handle: Some(store_alias.clone()),
        org_connection_name: None,
        manifest_digest: release_digest.clone(),
        component_digest: blob_digest,
        store_alias,
    })
    .await?;
    Ok(ReleaseCarrier {
        artifact_base: inputs.release_artifact_base.clone(),
        manifest_digest: wamn_catalog::ManifestDigest::parse(release_digest)?,
    })
}

#[expect(clippy::too_many_arguments)]
async fn author_wirings(
    route: &ProvisionedRoute,
    credentials: &JourneyCredentials,
    binary: &Path,
    package: &PackageCoordinate,
    registry: &PackageRegistry,
    system_url: &str,
    wirings: &[PathBuf],
    evidence: &Path,
) -> anyhow::Result<()> {
    let bind = "127.0.0.1:18080";
    let listener =
        std::net::TcpListener::bind(bind).context("reserve the WMS authoring address")?;
    drop(listener);
    let log = fs::File::create(evidence.join("authoring-gate.log"))?;
    let mut child = tokio::process::Command::new(binary)
        .args(["serve", "--bind", bind, "--insecure-registry"])
        .env("WAMN_PACKAGE_ARTIFACT_BASE", &registry.artifact_base)
        .env("WAMN_REGISTRY_AUTH_FILE", &registry.registry_auth_file)
        .env("WAMN_SYSTEM_URL", &credentials.identity_reader)
        .env("WAMN_CONTROL_AUTHORING_PG_URL", &credentials.control_author)
        .env(
            "WAMN_MANAGEMENT_ADMISSION_PG_URL",
            &credentials.management_admitter,
        )
        .env("WAMN_MANAGEMENT_ORG", identity().org.as_str())
        .env("WAMN_MANAGEMENT_PROJECT", identity().project.as_str())
        .env(
            "WAMN_MANAGEMENT_ENVIRONMENT",
            identity().environment.as_str(),
        )
        .env("WAMN_MANAGEMENT_TENANT", identity().tenant.as_str())
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
                        project_id: identity().project.clone(),
                        environment: identity().environment.clone(),
                    },
                },
                // The gate judges sealed ids, so the authored references
                // resolve first.
                &serde_json::to_string(
                    &wamn_schema_generator::operation_reference::read_authored_document(
                        path,
                        wamn_schema_generator::operation_reference::AuthoredDocument::Wiring,
                    )?,
                )?,
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
            author_wiring::author_wiring_in_databases(
                &route.database_url,
                system_url,
                registry,
                identity().tenant.as_str(),
                package.package_id(),
                package.package_version(),
                &document,
            )
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

/// The manifest of the WMS package of this checkout.
pub fn package_manifest() -> anyhow::Result<wamn_schema_generator::PackageManifest> {
    package_manifest_at(&package_root())
}

/// The manifest of the package at `root`.
pub fn package_manifest_at(root: &Path) -> anyhow::Result<wamn_schema_generator::PackageManifest> {
    let path = wamn_schema_generator::package_manifest_path(root);
    wamn_schema_generator::PackageManifest::from_slice(
        &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}

/// The materializer consumer of each registration of the WMS package
/// `manifest`, as the operator declares it to provisioning: the workflow
/// `movement_label` today.
///
/// The durable name is the one the materializer attaches to
/// (`mat_<tenant>_<package>_<registration>`), and the filter is the
/// registration entity's subjects in this environment.
pub fn declared_consumers(
    manifest: &wamn_schema_generator::PackageManifest,
) -> Vec<async_nats::jetstream::consumer::pull::Config> {
    let sanitize = |value: &str| {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    let handlers = manifest
        .custom_operations
        .iter()
        .filter_map(|(name, operation)| Some((name, operation.registration()?)));
    let workflows = manifest
        .workflows
        .iter()
        .filter_map(|(name, workflow)| Some((name, workflow.registration.as_ref()?)));
    handlers
        .chain(workflows)
        .map(|(name, registration)| {
            wamn_control_provision::events::materializer_consumer_config(
                &format!(
                    "mat_{}_{}_{}",
                    sanitize(identity().tenant.as_str()),
                    sanitize(&manifest.package.id),
                    sanitize(name)
                ),
                &format!(
                    "evt.{}.{}.{}.{}.>",
                    identity().org,
                    identity().project,
                    identity().environment,
                    wamn_event_wire::subject_token(&registration.entity)
                ),
                Duration::from_secs(30),
                5,
            )
        })
        .collect()
}

/// Enable the same CDC and registry-reader credentials as the WMS script,
/// with the materializer `consumers` of the installed package.
#[expect(clippy::too_many_arguments)]
pub async fn configure_cdc(
    inputs: &JourneyDocument,
    route: &ProvisionedRoute,
    work: &Path,
    replication_password: &str,
    nats_url: &str,
    provisioning: &wamn_test_infrastructure::event_broker::Credentials,
    source: &async_nats::jetstream::stream::Config,
    consumers: &[async_nats::jetstream::consumer::pull::Config],
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
    let args = EnableCdcProjectEnvRequest {
        org: identity().org.clone(),
        project: identity().project.clone(),
        env: identity().environment.clone(),
        schema: SCHEMA.into(),
        system_database_url: Some(inputs.system_pg_url.clone()),
        cluster: Some(CLUSTER.into()),
        replication_password: replication_password.into(),
        db_host: Some(host),
        db_port: database_config.get_ports().first().copied().unwrap_or(5432),
        namespace: inputs.host_secret_namespace.clone(),
        cluster_namespace: "platform".into(),
        secret_namespace: Some(inputs.host_secret_namespace.clone()),
        stream: None,
        nats_url: nats_url.to_owned(),
        nats_username: provisioning.username.clone(),
        nats_password_file: provisioning.password_file.clone(),
        stream_replicas: source.num_replicas,
        dup_window_secs: source.duplicate_window.as_secs(),
        consumer_config: consumers
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<_, _>>()?,
        emit_role_sql: Some(role_path),
        emit_cdc_sql: Some(cdc_path),
        emit_publication: None,
        emit_secret: Some(secret_path.clone()),
    };
    let (project, task) = connect(&route.database_url).await?;
    let result = shared::configure_cdc(args, project.as_ref(), project.as_ref()).await;
    drop(project);
    task.abort();
    result?;
    provision_project_env::run_workload_action(&generation_args(
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
