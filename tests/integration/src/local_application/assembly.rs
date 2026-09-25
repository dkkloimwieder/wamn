use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio_postgres::NoTls;
use wamn_catalog::{
    AttachmentTarget, ServingAttachment, ServingManifest, ServingRoute, WiringDocument,
};
use wamn_control::apply_package::{ApplyPackageRequest, apply_package};
use wamn_control::publish_release::package_route;
use wamn_control::push_component::{
    AdmitComponentRequest, admit_component, project_admitted_component_for_verification,
};
use wamn_control_provision::{
    CredentialGeneration, SystemReader, WorkloadRoleFamily, WorkloadRoleScope,
    system_reader_generation_role, workload_generation_role,
};
use wamn_engine::artifact_source::{LocalComponentSource, local_component_path};
use wamn_engine::engine::build_engine;
use wamn_engine::flow_http_routing::{FlowHttpRouting, RouteInFlightLimit};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_execution_host::{
    OperationHost, OperationScope, RouterDeliveryBridge, RouterReadinessProbe,
    RouterReadinessStatus,
};
use wamn_platform_identity::{
    assign_project_role, create_service, issue_pat, route_caller_subject,
};
use wamn_run_state::AuthorityClass;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::route_authentication::{
    PlatformRouteAuthenticator, RouteAuthentication,
};
use wamn_runtime::plugins::wamn_jetstream::{WamnJetstream, WamnJetstreamConfig};
use wamn_runtime::plugins::wamn_logging::WamnLoggingConfig;
use wamn_runtime::plugins::wamn_postgres::{
    ClassCredentials, ProjectConfig, StaticCredentialProvider, WamnPostgres,
};
use wamn_runtime::plugins::{WamnCredentials, WamnLogging};
use wamn_workflow::{RouterDriver, RouterDriverConfig, WiringCacheCapacity};
use wash_runtime::wasmtime::component::Component;

use super::{LocalApplicationConfig, LocalApplicationRuntime, PreparedLocalApplication};

const RELEASE_ID: i32 = 1;
const PASSWORD: &str = "local-application-test-only";
const VALID_UNTIL: &str = "2099-01-01T00:00:00Z";

#[derive(Debug)]
pub struct LocalPackage<'a> {
    pub root: &'a Path,
    pub component: &'a str,
    /// The wirings this package serves. A package whose attachments all
    /// target routes serves none.
    pub wirings: &'a [WiringDocument],
}

pub(super) async fn assemble(
    input: LocalApplicationConfig<'_>,
) -> anyhow::Result<PreparedLocalApplication> {
    let (system, system_connection) = tokio_postgres::connect(input.system_database_url, NoTls)
        .await
        .context("connect to the disposable identity database")?;
    let system_task = tokio::spawn(system_connection);
    wamn_control::dev::environment::reset_control_store(&system).await?;
    system
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string()],
        )
        .await?;
    system
        .execute(
            "INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ($1, 'pooled', 'local-application')",
            &[&input.org],
        )
        .await?;
    system
        .execute(
            "INSERT INTO registry.projects (org, id) VALUES ($1, $2)",
            &[&input.org, &input.project],
        )
        .await?;
    let subject = route_caller_subject(input.org, input.project, input.environment)?;
    let principal = create_service(&system, &subject, "local application route caller").await?;
    assign_project_role(
        &system,
        principal.id(),
        input.org,
        input.project,
        input.caller_role,
    )
    .await?;
    let issued = issue_pat(
        &system,
        principal.id(),
        "local application",
        Duration::from_secs(3600),
    )
    .await?;
    let bearer = issued.token().to_owned();

    let (project, project_connection) = tokio_postgres::connect(input.database_url, NoTls)
        .await
        .context("connect to the disposable application database")?;
    let project_task = tokio::spawn(project_connection);
    project
        .batch_execute(&wamn_control_provision::sql::set_database_owner_sql(
            &system_database_name(input.database_url)?,
        ))
        .await?;
    project
        .batch_execute(&format!(
            "{} {}",
            wamn_control_provision::sql::ensure_app_acl_role_sql(),
            wamn_schema_control::ensure_scenario_author_role_sql(),
        ))
        .await
        .context("create the catalog and application ACL owner roles")?;
    wamn_control::dev::environment::install_journey_platform_floor(
        &project,
        input.tenant,
        "local.invalid",
    )
    .await?;
    let run_schema = wamn_schema_control::BareSchemaName::new("wamn_run")?;
    wamn_control::reconcile_run_plane::reconcile(&project, &run_schema, true).await?;
    project
        .execute(
            "SELECT set_config('app.user_id',$1,false), set_config('app.operation','admin:local-application',false)",
            &[&wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string()],
        )
        .await?;
    project
        .execute(
            "INSERT INTO app_system.users (tenant_id,id,type,email) VALUES ($1,$2::text::uuid,'service','local@example.invalid')",
            &[&input.tenant, &principal.id().as_str()],
        )
        .await?;
    project
        .execute(
            "INSERT INTO app_system.roles (tenant_id,name) VALUES ($1,$2) ON CONFLICT DO NOTHING",
            &[&input.tenant, &input.caller_role],
        )
        .await?;
    project
        .execute(
            "INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ($1,$2::text::uuid,$3)",
            &[&input.tenant, &principal.id().as_str(), &input.caller_role],
        )
        .await?;

    let database = input
        .database_url
        .parse::<tokio_postgres::Config>()?
        .get_dbname()
        .context("application URL names no database")?
        .to_owned();
    let mut credentials = ClassCredentials::default();
    for (family, class) in [
        (WorkloadRoleFamily::App, AuthorityClass::GuestSql),
        (
            WorkloadRoleFamily::ExecutorPlatform,
            AuthorityClass::ExecutorPlatform,
        ),
        (
            WorkloadRoleFamily::HttpAdmitter,
            AuthorityClass::CallableHttp,
        ),
    ] {
        let scope = if family == WorkloadRoleFamily::App {
            WorkloadRoleScope::Tenant {
                tenant: input.tenant,
                database: &database,
            }
        } else {
            WorkloadRoleScope::ProjectEnvironment {
                org: input.org,
                project: input.project,
                environment: input.environment,
                database: &database,
            }
        };
        let role = workload_generation_role(family, scope, CredentialGeneration::A)?;
        project
            .batch_execute(
                &wamn_control_provision::sql::prepare_workload_generation_sql(
                    family,
                    &database,
                    &role,
                    PASSWORD,
                    VALID_UNTIL,
                ),
            )
            .await?;
        credentials = credentials.with_class(class, generation_url(input.database_url, &role)?);
    }
    let engine = Arc::new(build_engine(&[])?);
    project
        .execute(
            "INSERT INTO catalog.effective_releases (tenant_id,effective_release_id,environment,verified_publisher_principal) VALUES ($1,$2,$3,$4)",
            &[&input.tenant, &RELEASE_ID, &input.environment, &subject],
        )
        .await?;
    let mut component_digests = HashMap::new();
    let mut admitted = Vec::new();
    let mut wirings = Vec::new();
    let mut roots = BTreeMap::new();
    for package in input.packages {
        let applied = apply_package(ApplyPackageRequest {
            package: package.root.to_owned(),
            database_url: input.database_url.to_owned(),
            tenant: input.tenant.to_owned(),
        })
        .await?;
        let declaration_source = package
            .root
            .join("publication/components")
            .join(format!("{}.json.in", package.component));
        let declaration_path = input
            .scratch
            .join(format!("{}-{}.json", applied.package_id, package.component));
        render_declaration(&input, package, &declaration_source, &declaration_path)?;
        let component_path = input
            .component_directory
            .join(format!("{}.wasm", package.component));
        let admission = admit_component(AdmitComponentRequest {
            package: package.root.to_owned(),
            component_bytes: component_path.clone(),
            declaration: declaration_path,
            admitted_platform_packages: vec!["wamn:node".into(), "wamn:postgres".into()],
        })?;
        project_admitted_component_for_verification(&admission, input.database_url).await?;
        let facts = admission.facts().clone();
        project.execute("INSERT INTO catalog.effective_release_packages (tenant_id,effective_release_id,package_id,package_version) VALUES ($1,$2,$3,$4)", &[&input.tenant,&RELEASE_ID,&applied.package_id,&applied.package_version]).await?;
        std::fs::write(
            local_component_path(input.scratch, &facts.component_digest)?,
            std::fs::read(component_path)?,
        )?;
        component_digests.insert(package.component.to_owned(), facts.component_digest.clone());
        for document in package.wirings {
            insert_wiring(
                &project,
                input.tenant,
                &applied.package_id,
                &applied.package_version,
                document,
                &facts,
            )
            .await?;
            wirings.push((applied.package_id.clone(), document.clone()));
        }
        roots.insert(applied.package_id.clone(), package.root);
        admitted.push((applied.package_id, facts));
    }
    wamn_control::reconcile_package_data_access::reconcile_package_data_access(
        wamn_control::reconcile_package_data_access::ReconcilePackageDataAccessRequest {
            packages: input
                .packages
                .iter()
                .map(|package| package.root.to_owned())
                .collect(),
            database_url: input.database_url.to_owned(),
            tenant: input.tenant.to_owned(),
        },
    )
    .await?;

    let attachments = selected_attachments(&input, &roots)?;
    let routes = attachment_routes(&attachments, &roots)?;
    let manifest = serving_manifest(&input, &admitted, &wirings, &routes, &attachments)?;
    let canonical = manifest.canonical_bytes();
    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &canonical,
        "local application",
    )?);
    project.execute("INSERT INTO catalog.release_manifest_v3_snapshots (tenant_id,effective_release_id,manifest_digest,canonical_bytes) VALUES ($1,$2,$3,$4)", &[&input.tenant,&RELEASE_ID,&release.release().manifest_digest.as_str(),&canonical]).await?;

    let project_config = ProjectConfig {
        credentials,
        guest_pool_max_size: 4,
        platform_pool_max_size: 4,
        wait_timeout_ms: 5000,
        statement_timeout_ms: 10000,
        row_limit: 10000,
    };
    let postgres = Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(
            HashMap::from([(input.project.to_owned(), project_config)]),
            None,
        ),
    )));
    let driver = Arc::new(RouterDriver::new(
        Arc::new(OperationHost::new(
            Arc::clone(&engine),
            Arc::clone(&postgres),
            Arc::new(HttpTransport::new()?),
            Arc::new(WamnCredentials::empty()),
            Arc::new(WamnLogging::new(&WamnLoggingConfig::default())?),
            Arc::from([]),
            Arc::clone(&release),
            Arc::new(LocalComponentSource::new(input.scratch.to_owned())),
            OperationScope {
                project: input.project.to_owned(),
                schema: Some(input.schema.to_owned()),
                owner_prefix: "local-application".into(),
                warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
            },
        )?),
        RouterDriverConfig {
            cache_capacity: WiringCacheCapacity::default(),
        },
    ));
    let readiness = RouterReadinessProbe::new(
        driver.operations(),
        Some(Arc::clone(&driver) as Arc<dyn wamn_execution_host::WiringDelivery>),
    )
    .refresh()
    .await;
    anyhow::ensure!(
        readiness.status == RouterReadinessStatus::Ready,
        "local application release is not ready: {readiness:?}"
    );
    let jetstream = Arc::new(
        WamnJetstream::new(WamnJetstreamConfig {
            nats_url: None,
            ..WamnJetstreamConfig::default()
        })
        .with_release(Some(Arc::clone(&release))),
    );
    let bridge = Arc::new(RouterDeliveryBridge::new(
        driver.operations(),
        Some(driver as Arc<dyn wamn_execution_host::WiringDelivery>),
        jetstream,
        input.project,
    )?);
    let system_database = system_database_name(input.system_database_url)?;
    let identity_role = system_reader_generation_role(
        SystemReader::Identity,
        input.tenant,
        input.project,
        input.environment,
        &system_database,
        CredentialGeneration::A,
    );
    system
        .batch_execute(
            &wamn_control_provision::sql::prepare_workload_generation_sql(
                WorkloadRoleFamily::IdentityReader,
                &system_database,
                &identity_role,
                PASSWORD,
                VALID_UNTIL,
            ),
        )
        .await?;
    let (reader, reader_connection) = tokio_postgres::connect(
        &generation_url(input.system_database_url, &identity_role)?,
        NoTls,
    )
    .await?;
    tokio::spawn(reader_connection);
    let routing = Arc::new(
        FlowHttpRouting::new(Some(release), RouteInFlightLimit::default()).with_authenticator(
            Arc::new(
                PlatformRouteAuthenticator::default().with_authentication(Arc::new(
                    RouteAuthentication::new(
                        Arc::new(reader),
                        postgres,
                        input.org,
                        input.project,
                        subject,
                    )
                    .await?,
                )),
            ),
        ),
    );
    let flow_http = Component::new(engine.inner(), std::fs::read(input.flow_http_wasm)?)?;
    let secret = input.scratch.join("route-caller-pat.json");
    wamn_control::provision_project_env::write_secret_json(
        &secret,
        &json!({"stringData":{"token":bearer}}),
    )?;
    project_task.abort();
    system_task.abort();
    Ok(PreparedLocalApplication {
        runtime: LocalApplicationRuntime {
            engine,
            flow_http,
            routing,
            bridge,
        },
        route_host: input.route_host.to_owned(),
        bearer,
        caller_secret_path: secret,
        tenant: input.tenant.to_owned(),
        caller_role: input.caller_role.to_owned(),
        component_digests,
    })
}

fn render_declaration(
    input: &LocalApplicationConfig<'_>,
    package: &LocalPackage<'_>,
    source: &Path,
    target: &Path,
) -> anyhow::Result<()> {
    let mut base_digests =
        wamn_control::component_declaration::authored_base_digests(package.root)?;
    for base in input.packages {
        let manifest: Value = serde_json::from_slice(&std::fs::read(base.root.join("wamn.json"))?)?;
        let coordinate = format!(
            "{}@{}",
            manifest["package"]["id"].as_str().context("package id")?,
            manifest["package"]["version"]
                .as_str()
                .context("package version")?
        );
        if let Some(digest) = base_digests.get_mut(coordinate.as_str()) {
            let bytes = std::fs::read(
                input
                    .component_directory
                    .join(format!("{}.wasm", base.component)),
            )?;
            *digest = wamn_engine::component_admission::component_digest(&bytes).into_boxed_str();
        }
    }
    let value = wamn_control::component_declaration::render_declaration_document(
        source,
        input.tenant,
        &base_digests,
    )?;
    std::fs::write(target, serde_json::to_vec(&value)?)?;
    Ok(())
}
fn generation_url(base: &str, role: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(base)?;
    url.set_username(role)
        .map_err(|()| anyhow::anyhow!("set generation username"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set generation password"))?;
    Ok(url.to_string())
}
fn system_database_name(url: &str) -> anyhow::Result<String> {
    Ok(url
        .parse::<tokio_postgres::Config>()?
        .get_dbname()
        .context("system URL names no database")?
        .to_owned())
}

async fn insert_wiring(
    client: &tokio_postgres::Client,
    tenant: &str,
    package: &str,
    version: &str,
    document: &WiringDocument,
    component: &wamn_catalog::AdmittedComponent,
) -> anyhow::Result<()> {
    let graph_hash = document.wiring_hash();
    client.execute("INSERT INTO catalog.wirings (tenant_id,package_id,package_version,wiring_id,version,graph_json,wiring_hash) VALUES ($1,$2,$3,$4,$5,$6::text::jsonb,$7)",&[&tenant,&package,&version,&document.wiring_id,&i32::try_from(document.version)?,&serde_json::to_string(document)?,&graph_hash.as_str()]).await?;
    for (node_id, node) in &document.nodes {
        if node.component == component.component {
            client.execute("INSERT INTO catalog.release_components (tenant_id,effective_release_id,wiring_package_id,wiring_package_version,wiring_id,wiring_version,node_id,package_id,package_version,component_digest) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",&[&tenant,&RELEASE_ID,&package,&version,&document.wiring_id,&i32::try_from(document.version)?,node_id,&component.scope.package_id,&component.scope.package_version,&component.component_digest]).await?;
        }
    }
    Ok(())
}

fn serving_manifest(
    input: &LocalApplicationConfig<'_>,
    components: &[(String, wamn_catalog::AdmittedComponent)],
    wirings: &[(String, WiringDocument)],
    routes: &BTreeSet<ServingRoute>,
    attachments: &BTreeMap<String, ServingAttachment>,
) -> anyhow::Result<ServingManifest> {
    let packages = components
        .iter()
        .map(|(id, c)| json!({"package-id":id,"package-version":c.scope.package_version}))
        .collect::<Vec<_>>();
    // Publish folds each export's call graph, so each fact projects with the
    // facts it depends on.
    let resolve = |dependency: &wamn_catalog::ComponentOperationDependency| {
        components.iter().map(|(_, fact)| fact).find(|fact| {
            fact.scope.package_id == dependency.package
                && fact.component_digest == dependency.digest
        })
    };
    let components = components
        .iter()
        .map(|(_, component)| {
            Ok(serde_json::to_value(
                wamn_catalog::ServingComponent::project(component, &resolve)?,
            )?)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let wirings=wirings.iter().map(|(id,w)|json!({"package-id":id,"wiring-id":w.wiring_id,"wiring-version":w.version,"graph-hash":w.wiring_hash().as_str()})).collect::<Vec<_>>();
    let (attachments, workflow_attachments) = ServingAttachment::split(attachments.clone());
    Ok(serde_json::from_value(
        json!({"format-version":wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,"release":{"tenant-id":input.tenant,"effective-release-id":RELEASE_ID,"environment":input.environment,"packages":packages},"components":components,"routes":routes,"attachments":attachments,"workflow":{"wirings":wirings,"attachments":workflow_attachments}}),
    )?)
}

/// The attachments of the selected wirings, and every route of a selected
/// package.
fn selected_attachments(
    input: &LocalApplicationConfig<'_>,
    roots: &BTreeMap<String, &Path>,
) -> anyhow::Result<BTreeMap<String, ServingAttachment>> {
    let selected = input
        .packages
        .iter()
        .flat_map(|package| {
            package
                .wirings
                .iter()
                .map(|wiring| wiring.wiring_id.as_str())
        })
        .collect::<BTreeSet<_>>();
    let mut result = BTreeMap::new();
    for (id, attachment) in input.attachments {
        let served = match &attachment.target {
            AttachmentTarget::Wiring { wiring_id, .. } => selected.contains(wiring_id.as_str()),
            AttachmentTarget::Route { .. } => roots.contains_key(&attachment.package_id),
        };
        if served {
            let mut attachment = attachment.clone();
            // As publish does: a route to a read is a GET, and every other
            // route and every wiring is a POST.
            let method = match &attachment.target {
                AttachmentTarget::Route {
                    component,
                    operation,
                } => {
                    let root = roots
                        .get(&attachment.package_id)
                        .context("a route attachment names an assembled package")?;
                    package_route(root, &attachment.package_id, component, operation)?
                        .kind
                        .http_method()
                }
                AttachmentTarget::Wiring { .. } => "POST",
            };
            attachment.definition["route"]["method"] = Value::String(method.into());
            attachment.definition["route"]["host"] = Value::String(input.route_host.into());
            attachment.definition_hash = wamn_catalog::DefinitionHash::parse(
                wamn_execution_contract::canonical_json_sha256(&attachment.definition),
            )?;
            result.insert(id.clone(), attachment);
        }
    }
    Ok(result)
}

/// One manifest route for each route attachment, read from the package's
/// generated contract `operation.json` by the same function that publish uses.
fn attachment_routes(
    attachments: &BTreeMap<String, ServingAttachment>,
    roots: &BTreeMap<String, &Path>,
) -> anyhow::Result<BTreeSet<ServingRoute>> {
    let mut routes = BTreeSet::new();
    for attachment in attachments.values() {
        let AttachmentTarget::Route {
            component,
            operation,
        } = &attachment.target
        else {
            continue;
        };
        let root = roots
            .get(&attachment.package_id)
            .context("a route attachment names an assembled package")?;
        routes.insert(package_route(
            root,
            &attachment.package_id,
            component,
            operation,
        )?);
    }
    Ok(routes)
}
