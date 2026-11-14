use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_catalog::{AttachmentTarget, ServingAttachment, ServingManifest, ServingRoute};
use wamn_control_provision::{
    SystemReader, WorkloadRoleFamily, WorkloadRoleIdentity, system_reader_login, workload_login,
};
use wamn_engine::artifact_source::{LocalComponentSource, local_component_path};
use wamn_engine::engine::build_engine;
use wamn_engine::flow_http_routing::FlowHttpRouting;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_environment::apply_package::{ApplyPackageRequest, apply_package};
use wamn_execution_host::{
    OperationHost, OperationHostConfig, RouterReadinessProbe, RouterReadinessStatus,
};
use wamn_package_control::component_admission::{AdmitComponentRequest, admit_component};
use wamn_platform_identity::{assign_project_role, create_service, issue_pat, operator_subject};

use wamn_identity_protocol::AuthorityClass;
use wamn_release_control::publish_release::package_route;
use wamn_runtime::connection_credentials::ConnectionCredentials;
use wamn_runtime::plugins::WamnLogging;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::route_authentication::{
    PlatformRouteAuthenticator, RouteAuthentication,
};
use wamn_runtime::plugins::wamn_logging::WamnLoggingConfig;
use wamn_runtime::plugins::wamn_postgres::{
    ClassCredentials, ProjectConfig, StaticCredentialProvider, WamnPostgres,
};
use wamn_workflow::RouterDriver;
use wash_runtime::wasmtime::component::Component;

use super::{
    LocalApplicationConfig, LocalApplicationRuntime, LocalQueue, PreparedLocalApplication,
};

const PASSWORD: &str = "local-application-test-only";

#[derive(Debug)]
pub struct LocalPackage<'a> {
    pub root: &'a Path,
    pub component: &'a str,
}

pub(super) async fn assemble(
    input: LocalApplicationConfig<'_>,
) -> anyhow::Result<PreparedLocalApplication> {
    let environment = wamn_environment::environment::document::EnvironmentDocument::compile(
        &input.scratch.join("environment.k"),
    )?;
    let mut system = wamn_postgres::connect(input.system_database_url)
        .await
        .context("connect to the disposable identity database")?;

    wamn_dev_control::dev::environment::reset_control_store(&mut system).await?;
    sqlx::query("SELECT set_config('app.user_id', $1, false)")
        .bind(
            wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .execute(&mut system)
        .await
        .map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES ($1, 'pooled', 'local-application')").bind(input.org).execute(&mut system).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO registry.projects (org, id) VALUES ($1, $2)")
        .bind(input.org)
        .bind(input.project)
        .execute(&mut system)
        .await
        .map(|result| result.rows_affected())?;
    let subject = operator_subject(input.org, input.project, input.environment)?;
    let principal = create_service(
        &mut system,
        &subject,
        "local application operator",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await?;
    assign_project_role(
        &mut system,
        principal.id(),
        input.org,
        input.project,
        input.caller_role,
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await?;
    let issued = issue_pat(
        &mut system,
        principal.id(),
        "local application",
        Duration::from_secs(3600),
    )
    .await?;
    let bearer = issued.token().to_owned();

    let mut project = wamn_postgres::connect(input.database_url)
        .await
        .context("connect to the disposable application database")?;

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::set_database_owner_sql(&system_database_name(
            input.database_url,
        )?),
    ))
    .execute(&mut project)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::ensure_app_acl_role_sql(),
    ))
    .execute(&mut project)
    .await
    .map(drop)
    .context("create the catalog and application ACL owner roles")?;
    wamn_dev_control::dev::environment::install_journey_platform_base_grants(
        &mut project,
        "local.invalid",
    )
    .await?;
    let run_schema = wamn_postgres::schema::BareSchemaName::new("wamn_run")?;
    wamn_environment::platform_migrations::install_project(&mut project).await?;
    sqlx::query("SELECT set_config('app.user_id',$1,false), set_config('app.operation','admin:local-application',false)").bind(wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string()).execute(&mut project).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO platform.\"user\" (id,type,email) VALUES ($1::text::uuid,'service','local@example.invalid')").bind(principal.id().to_string().as_str()).execute(&mut project).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO platform.role (name) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(input.caller_role)
        .execute(&mut project)
        .await
        .map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO platform.user_role (user_id,role_name) VALUES ($1::text::uuid,$2)")
        .bind(principal.id().to_string().as_str())
        .bind(input.caller_role)
        .execute(&mut project)
        .await
        .map(|result| result.rows_affected())?;

    let mut database = input
        .database_url
        .parse::<sqlx::postgres::PgConnectOptions>()?
        .get_database()
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
        let target = if family == WorkloadRoleFamily::App {
            WorkloadRoleIdentity::ProjectEnvKey {
                project_env_key: input.project_env_key,
                database: &mut database,
            }
        } else {
            WorkloadRoleIdentity::ProjectEnvironment {
                org: input.org,
                project: input.project,
                environment: input.environment,
                database: &mut database,
            }
        };
        let role = workload_login(family, target)?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            wamn_control_provision::test_database::login_sql(family, &database, &role, PASSWORD),
        ))
        .execute(&mut project)
        .await
        .map(drop)?;
        credentials = credentials.with_class(class, generation_url(input.database_url, &role)?);
    }
    let engine = Arc::new(build_engine(&[])?);
    let mut component_digests = HashMap::new();
    let mut admitted = Vec::new();
    let mut admissions = Vec::new();
    let mut statement_instances = BTreeMap::new();
    let mut roots = BTreeMap::new();
    let migration_database_url = wamn_control_provision::test_database::package_migration_input(
        input.database_url,
        input.org,
        input.project,
        input.environment,
    )
    .await?;
    for package in input.packages {
        let applied = apply_package(ApplyPackageRequest {
            migration_database_url: migration_database_url.clone(),
            component_source: wamn_environment::apply_package::component_source_from_env(None)
                .unwrap(),
            changes: wamn_broker::Publisher::default(),
            package: wamn_environment::apply_package::PackageRoot {
                path: package.root.to_path_buf(),
                artifact_digest: Some(
                    wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(
                        package.root,
                    )?,
                ),
            },
            database_url: input.database_url.to_owned(),
            org: input.org.to_owned(),
            project: input.project.to_owned(),
            environment: input.environment.to_owned(),
        })
        .await?;
        let declaration_path = input
            .scratch
            .join(format!("{}-{}.json", applied.package_id, package.component));
        let base_digests = render_declaration(&input, package, &declaration_path)?;
        let component_path = input
            .component_directory
            .join(format!("{}.wasm", package.component));
        let admission = admit_component(AdmitComponentRequest {
            package: package.root.to_owned(),
            component_bytes: component_path.clone(),
            declaration: declaration_path,
            admitted_platform_packages: vec!["wamn:node".into(), "wamn:postgres".into()],
            base_digests,
        })?;
        let facts = admission.facts().clone();
        statement_instances.insert(
            facts.component_digest.clone(),
            admission.statement_instances().clone(),
        );
        std::fs::write(
            local_component_path(input.scratch, &facts.component_digest)?,
            std::fs::read(component_path)?,
        )?;
        component_digests.insert(package.component.to_owned(), facts.component_digest.clone());
        roots.insert(applied.package_id.clone(), package.root);
        admitted.push((applied.package_id, facts));
        admissions.push(admission);
    }

    let attachments = selected_attachments(&input, &roots)?;
    let routes = attachment_routes(&attachments, &roots)?;
    let mut manifest = serving_manifest(&admitted, &statement_instances, &routes, &attachments)?;
    manifest
        .host_routes
        .insert(wamn_catalog::HostRouteSet::Application);
    let canonical_bytes = manifest.canonical_bytes();
    let (_, digest) = ServingManifest::from_canonical_bytes(&canonical_bytes)?;
    let mut published = wamn_release_control::publish_release::PublishedRelease {
        manifest,
        digest,
        canonical_bytes,
    };
    let applications = environment
        .applications
        .into_iter()
        .map(|(name, application)| (name, application.into()))
        .collect();
    let entry = wamn_release_control::publish_release::compose_release_entry(
        &published,
        &admissions,
        &std::fs::read(input.flow_http_wasm)?,
        &applications,
        None,
    )?;
    let entry_path =
        wamn_engine::artifact_source::local_component_path(input.scratch, entry.digest())?;
    std::fs::write(&entry_path, entry.bytes())?;
    let entry_reference = format!("file:{}", entry_path.display());
    wamn_release_control::publish_release::record_composed_entry(
        &mut published,
        &entry,
        &entry_reference,
    )?;
    let canonical = published.canonical_bytes;
    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &canonical,
        "local application",
        wamn_engine::release_manifest::ReleaseTarget::new(input.project_env_key, input.environment)
            .with_applications(applications),
    )?);
    sqlx::query("INSERT INTO catalog.releases (project_env_key,manifest_digest,canonical_bytes) VALUES ($1,$2,$3)").bind(input.project_env_key).bind(release.release().manifest_digest.as_str()).bind(&canonical).execute(&mut project).await.map(|result| result.rows_affected())?;

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
    postgres.bind_operation_workload(
        super::LOCAL_WORKLOAD_ID,
        Arc::clone(&release),
        &entry_reference,
        "flow-http",
        wamn_runtime::plugins::wamn_postgres::SessionIdentity {
            project_env_key: input.project_env_key.to_owned(),
            project: Some(input.project.to_owned()),
            schema: Some(input.schema.to_owned()),
            runner: Some("local-application".into()),
            role: None,
            user_id: None,
            operation: None,
            application: None,
            release: Some(
                wamn_runtime::plugins::wamn_postgres::ReleaseIdentity::from_loaded(&release),
            ),
        },
    )?;
    let auth_nats = super::LocalAuthNats::start(input.scratch).await?;
    wamn_runtime::auth_cache::replicate(
        &auth_nats.client,
        Some(input.project_env_key),
        Arc::clone(&postgres),
    );
    let operations = Arc::new(OperationHost::new(
        Arc::clone(&engine),
        Arc::clone(&postgres),
        Arc::new(HttpTransport::new()?),
        Arc::new(wamn_runtime::connection_facts::ConnectionFacts::new(
            Arc::new(wamn_runtime::connection_memory::ConnectionMemory::default()),
            wamn_broker::Publisher::default(),
            Arc::new(ConnectionCredentials::empty()),
            Arc::clone(&postgres),
            &release.target().project_env_key,
            input.project,
            &release.target().environment,
        )),
        Arc::new(WamnLogging::new(&WamnLoggingConfig::default())?),
        Arc::from([]),
        Arc::clone(&release),
        Arc::new(LocalComponentSource::new(input.scratch.to_owned())),
        OperationHostConfig {
            project: input.project.to_owned(),
            schema: Some(input.schema.to_owned()),
            owner_prefix: "local-application".into(),
            warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
        },
    )?);
    let invoker: Arc<dyn wamn_runtime::plugins::wamn_postgres::OperationInvoker> =
        Arc::clone(&operations) as Arc<dyn wamn_runtime::plugins::wamn_postgres::OperationInvoker>;
    postgres.bind_operation_invoker(super::LOCAL_WORKLOAD_ID, Arc::downgrade(&invoker))?;
    let driver = Arc::new(RouterDriver::new(operations));
    let readiness = RouterReadinessProbe::new(driver.operations())
        .refresh()
        .await;
    anyhow::ensure!(
        readiness.status == RouterReadinessStatus::Ready,
        "local application release is not ready: {readiness:?}"
    );
    let bridge = Arc::new(wamn_execution_host::HostRouteDelivery::new(
        Arc::clone(&release),
        wamn_execution_host::HostRouteHandlers::Application {
            postgres: Arc::clone(&postgres),
            project: input.project.into(),
            identity: None,
            changes: wamn_broker::Publisher::default(),
        },
    ));
    let system_database = system_database_name(input.system_database_url)?;
    let identity_role = system_reader_login(
        SystemReader::Identity,
        input.project_env_key,
        input.project,
        input.environment,
        &system_database,
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::IdentityReader,
            &system_database,
            &identity_role,
            PASSWORD,
        ),
    ))
    .execute(&mut system)
    .await
    .map(drop)?;
    let reader = wamn_postgres::pool(
        &generation_url(input.system_database_url, &identity_role)?,
        4,
    )
    .await?;

    let local_resources = wash_runtime::types::LocalResources {
        config: HashMap::from([(
            wamn_catalog::http_routes::HTTP_ROUTES_CONFIG.into(),
            serde_json::to_string(&wamn_catalog::http_routes::route_data(
                release.manifest(),
                release.manifest().digest().as_str(),
            ))?,
        )]),
        ..Default::default()
    };
    let queue = LocalQueue {
        driver,
        postgres: Arc::clone(&postgres),
        release: Arc::clone(&release),
    };
    let routing = Arc::new(
        FlowHttpRouting::new(Some(release)).with_authenticator(Arc::new(
            PlatformRouteAuthenticator::default().with_authentication(Arc::new(
                RouteAuthentication::new(reader, Arc::clone(&postgres), input.project),
            )),
        )),
    );
    let flow_http = Component::new(engine.inner(), entry.bytes())?;
    let secret = input.scratch.join("operator-pat.json");
    wamn_environment::provision_project_env::write_secret_json(
        &secret,
        &json!({"stringData":{"token":bearer}}),
    )?;
    Ok(PreparedLocalApplication {
        auth_nats,
        runtime: LocalApplicationRuntime {
            engine,
            flow_http,
            plugins: vec![postgres],
            local_resources,
            routing,
            bridge,
            queue,
        },
        route_host: input.route_host.to_owned(),
        bearer,
        caller_secret_path: secret,
        project_env_key: input.project_env_key.to_owned(),
        caller_role: input.caller_role.to_owned(),
        component_digests,
    })
}

/// Render the declaration of `package` to `target`, and return the base
/// digests admission tests its composed base against: the authored pins,
/// with each base this run built layered over its pin.
fn render_declaration(
    input: &LocalApplicationConfig<'_>,
    package: &LocalPackage<'_>,
    target: &Path,
) -> anyhow::Result<BTreeSet<String>> {
    let mut base_digests =
        wamn_package_control::component_declaration::authored_base_digests(package.root)?;
    for base in input.packages {
        let manifest: Value = serde_json::from_slice(&std::fs::read(
            wamn_compiler::package_manifest_path(base.root),
        )?)?;
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
            *digest = wamn_digest::bytes_sha256(&bytes).into_boxed_str();
        }
    }
    let value = wamn_package_control::component_declaration::render_package_declaration(
        package.root,
        package.component,
        input.project_env_key,
    )?;
    std::fs::write(target, serde_json::to_vec(&value)?)?;
    Ok(base_digests.into_values().map(String::from).collect())
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
        .parse::<sqlx::postgres::PgConnectOptions>()?
        .get_database()
        .context("system URL names no database")?
        .to_owned())
}

fn serving_manifest(
    components: &[(String, wamn_catalog::AdmittedComponent)],
    statement_instances: &BTreeMap<String, BTreeMap<String, String>>,
    routes: &BTreeSet<ServingRoute>,
    attachments: &BTreeMap<String, ServingAttachment>,
) -> anyhow::Result<ServingManifest> {
    let packages = components
        .iter()
        .map(|(id, c)| json!({"package-id":id,"package-version":c.target.package_version}))
        .collect::<Vec<_>>();
    let components = components
        .iter()
        .map(|(_, component)| {
            // A composed component binds each member's named statements
            // import to that member's own set, as publish does.
            let base = |digest: &str| {
                components
                    .iter()
                    .map(|(_, fact)| fact)
                    .find(|fact| fact.component_digest == digest)
            };
            let served = wamn_catalog::ServingComponent::project(component, Vec::new())?
                .with_statement_instances(
                    statement_instances
                        .get(&component.component_digest)
                        .unwrap_or(&BTreeMap::new()),
                    &base,
                )?;
            Ok(serde_json::to_value(served)?)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let attachments = ServingAttachment::routes(attachments.clone());
    Ok(serde_json::from_value(
        json!({"format-version":wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,"release":{"packages":packages},"components":components,"routes":routes,"attachments":attachments}),
    )?)
}

/// The attachments of the selected wirings, and every route of a selected
/// package.
fn selected_attachments(
    input: &LocalApplicationConfig<'_>,
    roots: &BTreeMap<String, &Path>,
) -> anyhow::Result<BTreeMap<String, ServingAttachment>> {
    let mut result = BTreeMap::new();
    for (id, attachment) in input.attachments {
        let served = match &attachment.target {
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
                } => package_route(roots, &attachment.package_id, component, operation)?
                    .type_
                    .http_method(),
            };
            attachment.definition["route"]["method"] = Value::String(method.into());
            attachment.definition_hash = wamn_catalog::DefinitionHash::parse(
                wamn_digest::canonical_json_sha256(&attachment.definition),
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
        } = &attachment.target;
        routes.insert(package_route(
            roots,
            &attachment.package_id,
            component,
            operation,
        )?);
    }
    Ok(routes)
}
