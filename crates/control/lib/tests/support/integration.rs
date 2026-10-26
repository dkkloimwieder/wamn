//! Scheduled integration pull and CDC push in the environment kind case.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use wamn_catalog::{ComponentPackageScope, PackageCoordinate, WiringDocument};
use wamn_control::author_workflow::{
    AuthorWorkflowRequest, WorkflowExecution, WorkflowTrigger, author_workflow_in_databases,
};
use wamn_control::component_admission::{AdmitComponentRequest, admit_component};
use wamn_control::component_declaration::{
    authored_base_digests, render_declaration_document, render_palette_declaration,
};
use wamn_control::dev::environment::connect;
use wamn_control::environment::{self, Platform};
use wamn_control::package_artifact::{PackageRegistry, PushPackageRequest, push_package};
use wamn_control::publish_release::{PublishReleaseRequest, assemble_local_release};
use wamn_control::release_chart::ImageSet;
use wamn_control::workflow_lifecycle::{WorkflowVersion, disable_workflow, enable_workflow};
use wamn_control_provision::events::{advisory_stream_config, source_stream_config};
use wamn_control_provision::workload_role::WorkloadRoleFamily;
use wamn_test_infrastructure::event_broker;

use super::{
    ConnectionDefinition, DeclaredRelease, EnvironmentDocument, NAMESPACE, ORG, POLICY, PROJECT,
    Run, http_connection, qualification_artifact,
};

const ENVIRONMENT: &str = "integration";
const CONSUMER: &str = "rest_consumer_fixture";
/// The execution role of the connector's pull: exactly the consumer command
/// its graph calls.
pub const PULL_ROLE: &str = "connector-pull";
/// The user who authors and enables schedule workflows, as an environment
/// admin does. It holds each execution role it assigns.
const ENABLER: &str = "00000000-0000-4000-8000-0000000000e1";

/// One kind environment in which a test authors workflows.
pub struct WorkflowEnvironment<'a> {
    pub system_url: &'a str,
    pub project_url: &'a str,
    pub scope: &'a wamn_control_registry::Triple,
    pub tenant: &'a str,
    /// The release the environment serves.
    pub digest: &'a str,
    /// The applications the environment declares, as the serving Helm
    /// revision keeps them.
    pub applications: &'a wamn_catalog::EnvironmentApplications,
}

impl WorkflowEnvironment<'_> {
    fn version<'a>(&'a self, application: &'a str, workflow_id: &'a str) -> WorkflowVersion<'a> {
        WorkflowVersion {
            org: &self.scope.org,
            project: &self.scope.project,
            environment: &self.scope.env,
            application,
            workflow_id,
            version: 1,
        }
    }
}

/// The wiring of the connector's scheduled pull.
fn pull_wiring(connector: &str) -> &'static str {
    if connector == "netsuite" {
        "netsuite_pull"
    } else {
        "rest_item"
    }
}

/// One owned connection, as the workflow verbs take it.
async fn connect_owned(
    url: &str,
) -> anyhow::Result<(tokio_postgres::Client, tokio::task::JoinHandle<()>)> {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls).await?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, task))
}

/// Admit a manual fixture run through the application host route.
pub async fn start_workflow(
    environment: &WorkflowEnvironment<'_>,
    application: &str,
    workflow_id: &str,
    input: &Value,
    key: &str,
) -> anyhow::Result<String> {
    use std::sync::Arc;
    use wamn_engine::router_delivery::{
        DeliveryOutcome, DeliveryRequest, RouteDelivery as _, Source,
    };
    use wamn_runtime::plugins::wamn_postgres::{
        AuthorityClass, ClassCredentials, WamnPostgres, WamnPostgresConfig,
    };

    let (project, task) = connect_owned(environment.project_url).await?;
    project
        .execute(
            "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:schedule-fixture', false)",
            &[&wamn_project_state::PlatformComponent::Provisioning
                .principal_id()
                .to_string()],
        )
        .await?;
    project.execute(
        "INSERT INTO platform.user_role (user_id, role_name) VALUES ($1::text::uuid, 'admin') ON CONFLICT DO NOTHING",
        &[&ENABLER],
    ).await?;
    let bytes: Vec<u8> = project.query_one(
        "SELECT canonical_bytes FROM catalog.releases WHERE tenant_id=$1 AND manifest_digest=$2",
        &[&environment.tenant, &environment.digest],
    ).await?.get(0);
    task.abort();
    let release = Arc::new(
        wamn_engine::release_manifest::LoadedRelease::load_canonical_bytes(
            &bytes,
            "manual fixture workflow",
            wamn_engine::release_manifest::ReleaseScope::new(
                environment.tenant,
                environment.scope.env.as_str(),
            )
            .with_applications(environment.applications.clone()),
        )?,
    );
    let postgres = Arc::new(WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(
            ClassCredentials::default()
                .with_class(
                    AuthorityClass::ExecutorPlatform,
                    environment.project_url.to_owned(),
                )
                .with_class(
                    AuthorityClass::Administration,
                    environment.project_url.to_owned(),
                )
                .with_class(
                    AuthorityClass::CallableHttp,
                    environment.project_url.to_owned(),
                ),
        ),
        guest_pool_max_size: 2,
        platform_pool_max_size: 2,
        wait_timeout_ms: 5_000,
        statement_timeout_ms: 10_000,
        row_limit: 10_000,
    })?);
    let held = postgres
        .session_operation_permissions(
            "default",
            environment.tenant,
            &["admin".to_owned()],
            &ENABLER.parse()?,
        )
        .await?;
    let attachment = wamn_catalog::HostRouteSet::Application
        .attachments()
        .find(|(_, attachment)| attachment.reference == "wamn-control:workflow/start")
        .map(|(id, _)| id.to_owned())
        .context("workflow.start attachment")?;
    let caller = wamn_engine::flow_http_routing::AuthenticatedCaller::new(
        attachment.clone(),
        ENABLER,
        wamn_engine::flow_http_routing::CredentialType::Session,
        held.admin,
        held.references.iter().cloned().collect(),
        held.active_role(),
    );
    let host = wamn_execution_host::HostRouteDelivery::new(
        release,
        wamn_execution_host::HostRouteHandlers::Application {
            postgres,
            project: "default".to_owned(),
            identity: None,
            changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
        },
        None,
    );
    let report = host.deliver(DeliveryRequest {
        source: Source::Attachment(attachment), delivery_id: key.to_owned(),
        payload: json!([{"request_id":key, "value":{
            "application":application,"workflow_id":workflow_id,"input_json":input,"idempotency_key":key
        }}]).to_string(),
        caller: None, trace: None, parent_causation: None, if_none_match: None,
    }, Some(caller)).await;
    let Ok(DeliveryOutcome::Respond(body)) = report.outcome else {
        anyhow::bail!("workflow.start failed: {:?}", report.outcome);
    };
    let body: Value = serde_json::from_str(&body)?;
    body[0]["value"]["run_id"]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("workflow.start refused: {body}"))
}

/// Admit a manual fixture run as `workflow.start` admits it, for a release
/// that serves no application host routes. The HTTP environment has no
/// session issuer, so its release cannot serve them. The run records the
/// served release, so the deployed executor claims it.
pub async fn admit_workflow(
    environment: &WorkflowEnvironment<'_>,
    application: &str,
    workflow_id: &str,
    input: &Value,
    key: &str,
) -> anyhow::Result<String> {
    use wamn_runtime::plugins::wamn_postgres::{
        AuthorityClass, ClassCredentials, ManualRunAdmission, ManualRunAdmitted, WamnPostgres,
        WamnPostgresConfig,
    };

    let (project, task) = connect_owned(environment.project_url).await?;
    let bytes: Vec<u8> = project.query_one(
        "SELECT canonical_bytes FROM catalog.releases WHERE tenant_id=$1 AND manifest_digest=$2",
        &[&environment.tenant, &environment.digest],
    ).await?.get(0);
    task.abort();
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)?;
    let postgres = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::default().with_class(
            AuthorityClass::ExecutorPlatform,
            environment.project_url.to_owned(),
        )),
        guest_pool_max_size: 2,
        platform_pool_max_size: 2,
        wait_timeout_ms: 5_000,
        statement_timeout_ms: 10_000,
        row_limit: 10_000,
    })?;
    let workflow = postgres
        .enabled_workflow(
            "default",
            environment.tenant,
            environment.scope.env.as_str(),
            application,
            workflow_id,
        )
        .await?
        .context("the workflow is enabled")?;
    let role = workflow
        .execution_role
        .as_deref()
        .context("the fixture workflow runs with a role")?;
    let packages = &environment
        .applications
        .get(application)
        .context("the environment declares the application")?
        .packages;
    let node_digests: serde_json::Map<String, Value> = wamn_catalog::resolve_workflow_components(
        environment.tenant,
        packages,
        &manifest,
        &workflow.graph,
    )
    .map_err(|error| anyhow::anyhow!("{error:?}"))?
    .into_iter()
    .map(|(node, component)| (node, Value::String(component.component_digest)))
    .collect();
    let admitted = postgres
        .admit_manual_run(
            "default",
            environment.tenant,
            &ManualRunAdmission {
                package_id: application,
                manifest_digest: environment.digest,
                environment: environment.scope.env.as_str(),
                workflow_id,
                workflow_version: workflow.version,
                wiring_hash: &workflow.wiring_hash,
                execution_principal_id: &wamn_project_state::workflow_user_id().to_string(),
                execution_role: role,
                invoked_by_user_id: ENABLER,
                node_digests: &Value::Object(node_digests),
                idempotency_key: &format!("manual:{ENABLER}:{workflow_id}:{key}"),
                input,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let ManualRunAdmitted::Queued { run_id } = admitted else {
        anyhow::bail!("the fixture key {key} admitted no new run");
    };
    Ok(run_id)
}

/// The first minute boundary strictly after `after`.
pub fn next_minute(after: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(after.timestamp().div_euclid(60) * 60 + 60, 0)
        .expect("a minute boundary after a valid time is a valid time")
}

/// Author `document` as a minutely schedule workflow of `application` and
/// enable it, as an environment admin does with
/// `wamn-ctl workflow author --schedule '* * * * *'` and `enable`. The admin
/// first creates `role` with exactly `operations` and grants it to the
/// `workflow` user and to itself, so the runs execute as the `workflow` user
/// with `role`. A green gate report under the graph's hash stands in for the
/// gate.
pub async fn enable_schedule(
    environment: &WorkflowEnvironment<'_>,
    application: &str,
    document: &WiringDocument,
    role: &str,
    operations: &[&str],
) -> anyhow::Result<()> {
    enable_trigger(
        environment,
        application,
        document,
        role,
        operations,
        WorkflowTrigger::Schedule("* * * * *"),
        None,
    )
    .await
}

pub async fn enable_trigger(
    environment: &WorkflowEnvironment<'_>,
    application: &str,
    document: &WiringDocument,
    role: &str,
    operations: &[&str],
    trigger: WorkflowTrigger<'_>,
    events: Option<&environment::EventBroker>,
) -> anyhow::Result<()> {
    let tenant = environment.tenant;
    let (mut project, project_task) = connect_owned(environment.project_url).await?;
    project
        .execute(
            "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:schedule-fixture', false)",
            &[&wamn_project_state::PlatformComponent::Provisioning
                .principal_id()
                .to_string()],
        )
        .await?;
    project
        .execute(
            "INSERT INTO platform.\"user\" (id, type, email) \
             VALUES ($1::text::uuid, 'user', $2) ON CONFLICT (id) DO NOTHING",
            &[
                &ENABLER,
                &format!("schedule-enabler@{}", super::PLATFORM_DOMAIN),
            ],
        )
        .await?;
    project
        .execute("INSERT INTO platform.role (name) VALUES ($1)", &[&role])
        .await?;
    for operation in operations {
        project
            .execute(
                "INSERT INTO platform.permission (role_name, permission) \
                 VALUES ($1, $2)",
                &[&role, operation],
            )
            .await?;
    }
    let workflow_user = wamn_project_state::workflow_user_id().to_string();
    for user in [ENABLER, workflow_user.as_str()] {
        project
            .execute(
                "INSERT INTO platform.user_role (user_id, role_name) \
                 VALUES ($1::text::uuid, $2)",
                &[&user, &role],
            )
            .await?;
    }
    let (control, control_task) = connect_owned(environment.system_url).await?;
    control
        .execute("SELECT set_config('app.tenant', $1, false)", &[&tenant])
        .await?;
    control
        .execute(
            "INSERT INTO wamn_run.gate_reports (tenant_id, wiring_hash, passed, summary) \
             VALUES ($1, $2, true, '{}'::jsonb) ON CONFLICT DO NOTHING",
            &[&tenant, &document.wiring_hash().as_str()],
        )
        .await?;
    control_task.abort();
    author_workflow_in_databases(
        environment.project_url,
        environment.system_url,
        &AuthorWorkflowRequest {
            org: &environment.scope.org,
            project: &environment.scope.project,
            environment: &environment.scope.env,
            application,
            declared: environment.applications,
            document,
            trigger,
            execution: WorkflowExecution::Role(role),
            created_by: ENABLER,
            manifest_digest: environment.digest,
        },
    )
    .await?;
    enable_workflow(
        environment.system_url,
        environment.scope,
        &mut project,
        &environment.version(application, &document.wiring_id),
        ENABLER,
        environment.digest,
        environment.applications,
        events,
    )
    .await?;
    project_task.abort();
    Ok(())
}

/// Disable the schedule workflow `workflow_id` of `application`, as an
/// environment admin does with `wamn-ctl workflow disable`.
pub async fn disable_schedule(
    environment: &WorkflowEnvironment<'_>,
    application: &str,
    workflow_id: &str,
) -> anyhow::Result<()> {
    let (mut project, project_task) = connect_owned(environment.project_url).await?;
    disable_workflow(
        environment.system_url,
        environment.scope,
        &mut project,
        &environment.version(application, workflow_id),
    )
    .await?;
    project_task.abort();
    Ok(())
}

pub async fn exercise(
    run: &mut Run,
    repository: &Path,
    platform: &Platform,
    registry: &PackageRegistry,
    images: &ImageSet,
    admin_url: &str,
    connector: &str,
) -> anyhow::Result<()> {
    let scope = wamn_control_registry::Triple::new(ORG, PROJECT, ENVIRONMENT);
    let tenant = wamn_control_registry::project_env_tenant(ORG, PROJECT, ENVIRONMENT);
    let roots = [
        "apps/platform/wamn_integration",
        "apps/rest_consumer_fixture",
        &format!("apps/{connector}"),
    ]
    .map(|path| repository.join(path));
    let manifests: Vec<wamn_schema_generator::PackageManifest> = roots
        .iter()
        .map(|root| {
            Ok(serde_json::from_slice(&std::fs::read(
                wamn_schema_generator::package_manifest_path(root),
            )?)?)
        })
        .collect::<anyhow::Result<_>>()?;
    let source = source_stream_config(&scope, 1, Duration::from_secs(120));
    let consumers = wamn_control_provision::events::entity_consumers(&scope, &manifests);
    // Each connector run gets its own broker directory, because the broker
    // refuses an existing one.
    let broker_work = run.work.join(connector);
    std::fs::create_dir(&broker_work)?;
    let broker = event_broker::prepare(
        &broker_work,
        &scope,
        &tenant,
        &source,
        &advisory_stream_config(&scope, 1),
        &consumers,
    )?;
    let server = start_broker(run, &broker.configuration, &broker.provisioning, "events").await?;
    event_broker::write_binding(&broker, &server, &source)?;
    let mut platform = platform.clone();
    platform.events = Some(environment::EventBroker {
        nats_url: server.clone(),
        nats_username: broker.provisioning.username.clone(),
        nats_password_file: broker.provisioning.password_file.clone(),
        stream_replicas: 1,
        dup_window_secs: 120,
    });
    let mut document = EnvironmentDocument {
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENVIRONMENT.into(),
        release: DeclaredRelease::None,
        route_host: "integration.orders.example".into(),
        client_package: None,
        policy: POLICY.into(),
        // The existing consumer and integration packages put this application's
        // models in two schemas. The connector supplies the workflow components.
        applications: BTreeMap::from([(
            connector.to_owned(),
            wamn_catalog::ApplicationDefinition {
                packages: [connector, "wamn_integration", CONSUMER]
                    .map(str::to_owned)
                    .into(),
            },
        )]),
        connections: BTreeMap::new(),
        floors: BTreeMap::new(),
    };
    if connector == "netsuite" {
        wamn_control::org_connection::declare(&wamn_control::org_connection::DeclareConnectionRequest {
            system_database_url: platform.system_database_url.clone(),
            org: ORG.into(), name: "netsuite".into(),
            requirement_type: wamn_catalog::RequirementType::Http,
            definition: json!({"primary-authority":http_connection::ORIGIN, "tls-verification":"disabled"}),
        }).await?;
        document.connections.insert(
            "upstream".into(),
            ConnectionDefinition::Org {
                org: "netsuite".into(),
            },
        );
    } else {
        document.connections.insert("upstream".into(), ConnectionDefinition::Inline {
            requirement_type: wamn_catalog::RequirementType::Http,
            definition: json!({"primary-authority":http_connection::ORIGIN, "tls-verification":"disabled"}),
        });
    }
    // The preceding edge paging assertion used the same route. A fresh process
    // gives this independent flow its own page-two refusal and request log.
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "rollout",
            "restart",
            "deployment/http-fixture",
        ],
    )
    .await?;
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "rollout",
            "status",
            "deployment/http-fixture",
            "--timeout=120s",
        ],
    )
    .await?;
    let file = run.work.join("integration.k");
    std::fs::write(&file, document.to_kcl())?;
    // Platform install provisions with release=none before it renders the
    // session audience from the environment's allocated instance suffix.
    environment::apply(&platform, &file).await?;

    let (system, system_task) = connect(&platform.system_database_url).await?;
    let suffix: String = system.query_one(
        "SELECT instance_suffix FROM registry.project_envs WHERE org=$1 AND project=$2 AND env=$3",
        &[&ORG, &PROJECT, &ENVIRONMENT],
    ).await?.get(0);
    let issuer = if connector == "netsuite" {
        // The preceding Epic 4 case installed this issuer in the shared fixture.
        format!("https://integration-session.{NAMESPACE}.svc.cluster.local")
    } else {
        super::session_issuer::deploy(run, repository, &platform).await?
    };
    platform.host_group =
        Some(event_host_group(run, &platform, &broker, &server, &issuer, &suffix).await?);
    let mut project_url = url::Url::parse(admin_url)?;
    project_url.set_path(&format!(
        "/{}",
        wamn_control_provision::project_env_database_name(ORG, PROJECT, ENVIRONMENT, &suffix)
    ));
    let bytes = publish(
        run, repository, registry, &roots, &manifests, &tenant, connector,
    )
    .await?;
    let pushed = wamn_control::publish_release::push_manifest_bytes(
        &bytes,
        &platform.release_artifact_base,
        false,
        &platform.oci_ca_paths,
        &platform.registry_auth_file,
    )
    .await?;
    let digest = pushed.digest.as_str().to_owned();
    let mut releases = registry.clone();
    releases
        .artifact_base
        .clone_from(&platform.release_artifact_base);
    qualification_artifact::push(
        &releases,
        &qualification_artifact::QualificationPayload {
            format: qualification_artifact::QUALIFICATION_PAYLOAD_FORMAT,
            release_digest: digest.clone(),
            image_set: qualification_artifact::image_set_digests(images)?,
            cases: Vec::new(),
        },
        bytes.len(),
    )
    .await?;

    document.release = DeclaredRelease::Digest(digest.clone());
    super::apply_with_diagnostics(&run.work, &platform, &document, "integration").await?;
    let (project, project_task) = connect(project_url.as_str()).await?;
    let report_start = run.report.len();
    let capture_result = async {
    let capture = system
        .query_one(
            "SELECT schema, publication, slot FROM registry.event_readers WHERE org=$1 AND project=$2 AND env=$3",
            &[&ORG, &PROJECT, &ENVIRONMENT],
        )
        .await?;
    let captured: Vec<String> = capture.get(0);
    run.collect("4 integration capture", (|| {
    ensure!(
        captured == [CONSUMER, "wamn_integration"],
        "CDC must capture every installed application schema: {captured:?}"
    );
    Ok(())
    })());
    let publication: String = capture.get(1);
    let slot: String = capture.get(2);
    let publications = project
        .query("SELECT pubname::text FROM pg_publication", &[])
        .await?;
    let slots = project.query(
        "SELECT slot_name::text FROM pg_replication_slots WHERE database = current_database() AND slot_type = 'logical'",
        &[],
    ).await?;
    run.collect("4 integration capture", (|| {
    ensure!(
        publications.len() == 1
            && publications[0].get::<_, String>(0) == publication
            && slots.len() == 1
            && slots[0].get::<_, String>(0) == slot,
        "the environment must have exactly its registered publication and logical slot"
    );
    Ok(())
    })());
    let schemas: Vec<String> = project
        .query_one(
            "SELECT array_agg(n.nspname::text ORDER BY n.nspname) FROM pg_publication_namespace pn \
         JOIN pg_namespace n ON n.oid = pn.pnnspid \
         JOIN pg_publication p ON p.oid = pn.pnpubid WHERE p.pubname = $1",
            &[&publication],
        )
        .await?
        .get(0);
    run.collect("4 integration capture", (|| {
    ensure!(
        schemas == captured,
        "publication schemas differ: {schemas:?}"
    );
    Ok(())
    })());
    // Capture maps identify application entities. History and control tables
    // in the same schemas are explicitly excluded from entity events.
    let tables: Value = project
        .query_one(
            "SELECT jsonb_agg(jsonb_build_object('schema', n.nspname, 'table', c.relname, \
         'identity', c.relreplident, 'published', EXISTS (SELECT FROM pg_publication_tables p \
         WHERE p.pubname = $1 AND p.schemaname = n.nspname AND p.tablename = c.relname)) \
         ORDER BY n.nspname, c.relname) \
         FROM (SELECT relation_oid FROM rest_consumer_fixture.wamn_entities \
               UNION ALL SELECT relation_oid FROM wamn_integration.wamn_entities) e \
         JOIN pg_class c ON c.oid = e.relation_oid JOIN pg_namespace n ON n.oid = c.relnamespace",
            &[&publication],
        )
        .await?
        .get(0);
    run.collect("4 integration capture", (|| {
    ensure!(
        tables
            == json!([
                {"schema":CONSUMER,"table":"item","identity":"f","published":true},
                {"schema":"wamn_integration","table":"log","identity":"f","published":true},
                {"schema":"wamn_integration","table":"setting","identity":"f","published":true}
            ]),
        "every captured table must be published with full replica identity: {tables}"
    );
    Ok(())
    })());
    run.pass(
        "one environment publication and slot capture both application schemas",
        "SELECT registry.event_readers; SELECT pg_publication, pg_publication_namespace, pg_publication_tables, pg_class, pg_replication_slots",
        json!({"schemas":schemas,"publication":publication,"slot":slot,"tables":tables}),
        run.report[report_start..].iter().all(|row| row["result"] != "fail"),
    );
    Ok(())
    }.await;
    run.collect("4 integration capture", capture_result);
    if connector == "netsuite" {
        super::netsuite::seed(&project).await?;
    }
    let mut reader = start_reader(run, repository, &broker, &server, admin_url).await?;
    // The release declares no schedule. The pull is an environment workflow
    // of the connector, authored and enabled after apply
    // (docs/plan/workflows.md epic 2).
    let workflows = WorkflowEnvironment {
        system_url: &platform.system_database_url,
        project_url: project_url.as_str(),
        scope: &scope,
        tenant: &tenant,
        digest: &digest,
        applications: &environment::show::serving_applications(&platform, &scope).await?,
    };
    let read = |name: &str| -> anyhow::Result<WiringDocument> {
        let mut value = wamn_schema_generator::operation_reference::read_authored_document(
            &repository.join(format!("apps/{connector}/publication/wirings/{name}.json")),
            wamn_schema_generator::operation_reference::AuthoredDocument::Wiring,
        )?;
        if connector == "netsuite" && name == "netsuite_pull" {
            value["nodes"]["page"]["params"]["path-and-query"] =
                json!("/services/rest/query/v1/suiteql?limit=2");
        }
        Ok(serde_json::from_value(value)?)
    };
    let pull = read(pull_wiring(connector))?;
    let push = read(if connector == "netsuite" {
        "netsuite_push"
    } else {
        "rest_item_pushed"
    })?;
    let events = environment::EventBroker {
        nats_url: server.clone(),
        nats_username: broker.provisioning.username.clone(),
        nats_password_file: broker.provisioning.password_file.clone(),
        stream_replicas: source.num_replicas,
        dup_window_secs: source.duplicate_window.as_secs(),
    };
    let observed = async {
        enable_trigger(
            &workflows,
            connector,
            &push,
            "connector-push",
            &[],
            WorkflowTrigger::Event {
                source_package: CONSUMER,
                entity: "item",
                ops: &[wamn_event_wire::Op::Insert],
                condition: None,
            },
            Some(&events),
        )
        .await?;

        enable_schedule(
            &workflows,
            connector,
            &pull,
            PULL_ROLE,
            &["rest-consumer-fixture:item/record"],
        )
        .await?;
        if connector == "netsuite" {
            super::netsuite::assertions(run, &project, &workflows).await
        } else {
            let observed = assertions(run, &project, &tenant, &digest, &mut reader).await?;
            disable_schedule(&workflows, connector, &pull.wiring_id).await?;
            assert_schema_triggers(run, &project, &workflows, &events, &broker, &mut reader)
                .await?;
            Ok(observed)
        }
    }
    .await;
    reader.kill().await.context("stop the fixture CDC reader")?;
    reader.wait().await?;
    project_task.abort();
    system_task.abort();
    let observed = observed.with_context(|| {
        format!(
            "CDC reader log: {}",
            std::fs::read_to_string(run.work.join("cdc-reader.log")).unwrap_or_default()
        )
    });
    match observed {
        Ok(observed) => {
            run.pass(
        &format!(
            "{connector} scheduled pull as the workflow user with its role and consumer event push"
        ),
        "author and enable the pull's schedule workflow after apply; SELECT runs, item, setting and command results; GET /recorded",
        observed,
        true,
    );
        }
        Err(error) => run.collect(
            &format!("4 {connector} scheduled pull and events"),
            Err(error),
        ),
    }
    environment::delete::delete(&platform, &scope, true).await?;
    run.run("docker", &["rm", "-f", &format!("{}-events", run.cluster)])
        .await?;
    Ok(())
}

async fn publish(
    run: &Run,
    repository: &Path,
    registry: &PackageRegistry,
    roots: &[PathBuf],
    manifests: &[wamn_schema_generator::PackageManifest],
    tenant: &str,
    connector: &str,
) -> anyhow::Result<Vec<u8>> {
    let mut admissions = Vec::new();
    for (root, manifest) in roots.iter().zip(manifests) {
        push_package(&PushPackageRequest {
            package: root.clone(),
            registry: registry.clone(),
            component_artifact_base: format!(
                "{}/wamn/components",
                registry
                    .artifact_base
                    .strip_suffix("/wamn/packages")
                    .context("fixture package registry")?
            ),
        })
        .await?;
        let scope = ComponentPackageScope {
            tenant_id: tenant.into(),
            package_id: manifest.package.id.clone(),
            package_version: manifest.package.version.clone(),
        };
        let names = if manifest.package.id == connector {
            if connector == "netsuite" {
                vec![connector, "http-request", "http-page", "jsonata"]
            } else {
                vec![connector, "http-page", "jsonata"]
            }
        } else {
            vec![manifest.package.id.as_str()]
        };
        for name in names {
            let declaration = match name {
                "http-request" | "http-page" | "jsonata" => render_palette_declaration(
                    &repository.join(if name == "http-request" {
                        "apps/platform/no-std/http-request/declaration.json.in"
                    } else if name == "http-page" {
                        "apps/platform/no-std/http-page/declaration.json.in"
                    } else {
                        "apps/platform/execution/jsonata/declaration.json.in"
                    }),
                    &scope,
                    None,
                )?,
                _ => render_declaration_document(
                    &root.join(format!("publication/components/{name}.json.in")),
                    tenant,
                )?,
            };
            let file = run.work.join(format!("declaration-{name}.json"));
            std::fs::write(&file, serde_json::to_vec(&declaration)?)?;
            admissions.push(admit_component(AdmitComponentRequest {
                package: root.clone(),
                component_bytes: wamn_schema_generator::output_root(root)
                    .join(format!("components/{name}.wasm")),
                declaration: file,
                admitted_platform_packages: vec![
                    "wamn:node".into(),
                    "wamn:postgres".into(),
                    "wamn:connection".into(),
                ],
                base_digests: authored_base_digests(root)?
                    .into_values()
                    .map(String::from)
                    .collect(),
            })?);
        }
    }
    let request = PublishReleaseRequest {
        applications: [(
            connector.to_owned(),
            wamn_catalog::ApplicationDefinition {
                packages: manifests.iter().map(|m| m.package.id.clone()).collect(),
            },
        )]
        .into(),
        application: None,
        database_url: String::new(),
        control_database_url: String::new(),
        org: ORG.into(),
        project: PROJECT.into(),
        environment: ENVIRONMENT.into(),
        verified_publisher_principal: "kind-fixture".into(),
        run_schema: "wamn_run".into(),
        packages: manifests
            .iter()
            .map(|m| PackageCoordinate::new(&m.package.id, &m.package.version))
            .collect::<Result<_, _>>()?,
    };
    Ok(assemble_local_release(&request, roots, &admissions)?
        .published
        .canonical_bytes)
}

pub(super) async fn start_broker(
    run: &Run,
    configuration: &Path,
    credentials: &event_broker::Credentials,
    suffix: &str,
) -> anyhow::Result<String> {
    let name = format!("{}-{suffix}", run.cluster);
    run.run(
        "docker",
        &[
            "run",
            "--detach",
            "--name",
            &name,
            "--network",
            "kind",
            "--volume",
            &format!("{}:/fixture/nats.conf:ro", configuration.display()),
            "nats:2.10-alpine",
            "--jetstream",
            "--store_dir",
            "/data",
            "--config",
            "/fixture/nats.conf",
        ],
    )
    .await?;
    let ip = run
        .run(
            "docker",
            &[
                "inspect",
                "--format",
                "{{(index .NetworkSettings.Networks \"kind\").IPAddress}}",
                &name,
            ],
        )
        .await?;
    let server = format!("nats://{}:4222", ip.trim());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match event_broker::connect(credentials, &server).await {
            Ok(_) => return Ok(server),
            Err(_) if Instant::now() < deadline => tokio::time::sleep(Duration::from_secs(1)).await,
            Err(error) => return Err(error),
        }
    }
}

async fn event_host_group(
    run: &Run,
    platform: &Platform,
    broker: &event_broker::EventBroker,
    server: &str,
    issuer: &str,
    suffix: &str,
) -> anyhow::Result<PathBuf> {
    let secret = run.work.join("integration-events.json");
    std::fs::write(
        &secret,
        serde_json::to_vec(&json!({
            "apiVersion":"v1", "kind":"Secret", "metadata":{"name":"integration-events","namespace":NAMESPACE},
            "stringData":{"password":std::fs::read_to_string(&broker.runtime.password_file)?, "binding.json":std::fs::read_to_string(&broker.binding)?}
        }))?,
    )?;
    run.run("kubectl", &["apply", "-f", &secret.to_string_lossy()])
        .await?;
    let mut group: Value = serde_yaml::from_slice(&std::fs::read(
        platform.host_group.as_ref().context("host group")?,
    )?)?;
    group["env"]
        .as_array_mut()
        .context("host environment")?
        .extend(
            [
                ("WAMN_SESSION_ISSUER", issuer),
                ("WAMN_SESSION_JWKS_CA", "/integration-session/ca.crt"),
                ("WAMN_SESSION_INSTANCE_SUFFIX", suffix),
                ("WAMN_EVT_NATS_URL", server),
                ("WAMN_EVT_NATS_USERNAME", &broker.runtime.username),
                (
                    "WAMN_EVT_NATS_PASSWORD_FILE",
                    "/integration-events/password",
                ),
                (
                    "WAMN_MAT_NATS_BINDING_FILE",
                    "/integration-events/binding.json",
                ),
                ("WAMN_EVT_ORG", ORG),
                ("WAMN_EVT_PROJECT", PROJECT),
                ("WAMN_EVT_ENV", ENVIRONMENT),
                ("WAMN_EVT_STREAM_REPLICAS", "1"),
                ("WAMN_EVT_DUP_WINDOW_SECS", "120"),
            ]
            .map(|(name, value)| json!({"name":name, "value":value})),
        );
    group["volumes"]
        .as_array_mut()
        .context("host volumes")?
        .extend([
            json!({"name":"integration-events","secret":{"secretName":"integration-events"}}),
            json!({"name":"integration-session","configMap":{"name":"integration-session-ca"}}),
        ]);
    group["volumeMounts"]
        .as_array_mut()
        .context("host mounts")?
        .extend([
            json!({"name":"integration-events","mountPath":"/integration-events","readOnly":true}),
            json!({"name":"integration-session","mountPath":"/integration-session","readOnly":true}),
        ]);
    let file = run.work.join("integration-host-group.yaml");
    std::fs::write(&file, serde_yaml::to_string(&group)?)?;
    Ok(file)
}

fn binary(repository: &Path, name: &str) -> PathBuf {
    repository
        .join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
        .join("debug")
        .join(name)
}

async fn secret_value(run: &Run, name: &str, key: &str) -> anyhow::Result<Vec<u8>> {
    let value = run
        .json(
            "kubectl",
            &["-n", NAMESPACE, "get", "secret", name, "-o", "json"],
        )
        .await?;
    Ok(STANDARD.decode(value["data"][key].as_str().context("Secret key")?)?)
}

async fn start_reader(
    run: &Run,
    repository: &Path,
    broker: &event_broker::EventBroker,
    server: &str,
    admin_url: &str,
) -> anyhow::Result<Child> {
    let forward = url::Url::parse(admin_url)?;
    let names = [
        wamn_control_provision::workload_secret_name(
            WorkloadRoleFamily::RegistryReader,
            ORG,
            PROJECT,
            ENVIRONMENT,
        ),
        wamn_control_provision::project_env_cdc_secret_name(ORG, PROJECT, ENVIRONMENT),
    ];
    let mut urls = Vec::new();
    for name in names {
        let bytes = secret_value(run, &name, "url").await?;
        let mut url = url::Url::parse(std::str::from_utf8(&bytes)?)?;
        url.set_host(forward.host_str())?;
        url.set_port(forward.port())
            .map_err(|()| anyhow::anyhow!("reader database port"))?;
        urls.push(url.to_string());
    }
    let log = std::fs::File::create(run.work.join("cdc-reader.log"))?;
    let child = Command::new(binary(repository, "wamn-cdc-reader"))
        .env("WAMN_SYSTEM_URL", &urls[0])
        .env("WAMN_CDC_URL", &urls[1])
        .env("WAMN_EVT_NATS_URL", server)
        .env("WAMN_EVT_NATS_USERNAME", &broker.publisher.username)
        .env(
            "WAMN_EVT_NATS_PASSWORD_FILE",
            &broker.publisher.password_file,
        )
        .args([
            "--org",
            ORG,
            "--project",
            PROJECT,
            "--env",
            ENVIRONMENT,
            "--stream-replicas",
            "1",
            "--dup-window-secs",
            "120",
            "--feedback-secs",
            "1",
            "--slot-poll-secs",
            "0",
        ])
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .context("start production CDC reader")?;
    Ok(child)
}

/// Observe real CDC deliveries before comparing their IDs with workflow runs.
async fn assert_schema_triggers(
    run: &mut Run,
    project: &tokio_postgres::Client,
    workflows: &WorkflowEnvironment<'_>,
    events: &environment::EventBroker,
    broker: &event_broker::EventBroker,
    reader: &mut Child,
) -> anyhow::Result<()> {
    let triggers = [
        (CONSUMER, "item", "capture-item"),
        ("wamn_integration", "setting", "capture-setting"),
    ];
    for (package, entity, workflow) in triggers {
        let document: WiringDocument = serde_json::from_value(json!({
            "format-version":"0.1", "wiring-id":workflow, "version":1, "entry":"observe",
            "nodes":{"observe":{"component":"jsonata", "interface-version":"0.1.0",
                "operation":"wamn:node/handler@0.1.0", "params":{"expression":"$"}}},
            "edges":[]
        }))?;
        enable_trigger(
            workflows,
            "rest_connector",
            &document,
            workflow,
            &[],
            WorkflowTrigger::Event {
                source_package: package,
                entity,
                ops: &[wamn_event_wire::Op::Update],
                condition: None,
            },
            Some(events),
        )
        .await?;
    }
    // The roles are granted in SQL, which drops no auth key, and the earlier
    // REST runs left the workflow user's grants in the host's memory. Wait
    // until that memory expires.
    tokio::time::sleep(wamn_platform_identity::auth_changes::AUTH_TTL).await;
    let jetstream = async_nats::jetstream::new(
        event_broker::connect(&broker.observer, &events.nats_url).await?,
    );
    let mut stream = jetstream
        .get_stream(wamn_control_provision::event_stream_name(
            ORG,
            PROJECT,
            ENVIRONMENT,
        ))
        .await?;
    let mut sequence = stream.info().await?.state.last_sequence + 1;
    project
        .batch_execute("BEGIN; SET LOCAL app.operation = 'admin:capture-fixture'")
        .await?;
    project
        .execute("SELECT set_config('app.user_id', $1, true)", &[&ENABLER])
        .await?;
    let txid: i64 = project
        .query_one(
            "SELECT pg_current_xact_id()::text::bigint % 4294967296",
            &[],
        )
        .await?
        .get(0);
    ensure!(
        project
            .execute(
                "UPDATE rest_consumer_fixture.item SET name = 'Capture changed' WHERE key = 'one'",
                &[],
            )
            .await?
            == 1,
        "change exactly one consumer item"
    );
    ensure!(project.execute(
        "UPDATE wamn_integration.setting SET value = '\"capture-changed\"'::jsonb WHERE key = 'last_synced_at'", &[],
    ).await? == 1, "change exactly one integration setting");
    project.batch_execute("COMMIT").await?;

    let ids = triggers.map(|(_, _, workflow)| workflow.to_owned());
    let mut deliveries = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        ensure!(
            reader.try_wait()?.is_none(),
            "CDC reader exited; read cdc-reader.log"
        );
        let tail = stream.info().await?.state.last_sequence;
        while sequence <= tail {
            let message = stream.get_raw_message(sequence).await?;
            let envelope: Value = serde_json::from_slice(&message.payload)?;
            if envelope["txid"] == txid {
                let (package, entity, workflow) = triggers
                    .iter()
                    .find(|(package, entity, _)| {
                        envelope["package_id"] == *package && envelope["entity"] == *entity
                    })
                    .context("the changed transaction emits only the two captured entities")?;
                ensure!(envelope["op"] == "update", "expected an update: {envelope}");
                ensure!(
                    message.subject.as_str()
                        == wamn_event_wire::subject(
                            ORG,
                            PROJECT,
                            ENVIRONMENT,
                            entity,
                            wamn_event_wire::Op::Update,
                        ),
                    "the event subject must identify its entity"
                );
                let key = if *package == CONSUMER {
                    "one"
                } else {
                    "last_synced_at"
                };
                ensure!(
                    envelope["new"]["key"] == key,
                    "wrong changed row: {envelope}"
                );
                let lsn = envelope["lsn"].as_u64().context("event LSN")?;
                let delivery = format!(
                    "event:{sequence}:{}",
                    wamn_event_wire::msg_id(PROJECT, ENVIRONMENT, lsn)
                );
                ensure!(
                    deliveries.insert(*workflow, (sequence, delivery)).is_none(),
                    "each row change must produce exactly one delivery"
                );
            }
            sequence += 1;
        }
        let rows: Value = project.query_one(
            "SELECT COALESCE(jsonb_agg(jsonb_build_object('workflow', wiring_id, 'delivery', invoked_by, \
             'status', status, 'trigger', trigger_source, 'fail_type', fail_type, \
             'result', result_json) ORDER BY wiring_id), '[]') \
             FROM wamn_run.runs WHERE tenant_id = $1 AND package_id = 'rest_connector' AND wiring_id = ANY($2)",
            &[&workflows.tenant, &&ids[..]],
        ).await?.get(0);
        let runs = rows.as_array().context("capture trigger runs")?;
        ensure!(runs.len() <= 2, "duplicate or cross-schema runs: {rows}");
        if deliveries.len() == 2 && runs.len() == 2 {
            let mut settled = true;
            for ((package, entity, workflow), row) in triggers.iter().zip(runs) {
                let (event_sequence, delivery) = &deliveries[workflow];
                ensure!(
                    row["workflow"] == *workflow
                        && row["delivery"] == *delivery
                        && row["trigger"] == "event",
                    "wrong trigger or cross-schema delivery: {rows}"
                );
                ensure!(
                    !matches!(
                        row["status"].as_str(),
                        Some("failed" | "infrastructure-failure" | "effect-uncertain")
                    ),
                    "capture workflow failed: {rows}"
                );
                settled &= row["status"] == "completed";
                let durable = format!("entity_{}_{}_{}", package.len(), package, entity);
                let mut consumer = stream
                    .get_consumer::<async_nats::jetstream::consumer::pull::Config>(&durable)
                    .await
                    .map_err(anyhow::Error::from_boxed)?;
                let info = consumer.info().await?;
                settled &= info.ack_floor.stream_sequence >= *event_sequence
                    && info.num_ack_pending == 0
                    && info.num_pending == 0;
            }
            if settled {
                run.pass(
                    "changes in both schemas each reach only their own trigger once",
                    "author and enable two event workflows; UPDATE item; UPDATE setting; read retained CDC deliveries; SELECT wamn_run.runs",
                    json!({"transaction":txid,"deliveries":deliveries,"runs":rows}),
                    true,
                );
                return Ok(());
            }
        }
        ensure!(
            Instant::now() < deadline,
            "schema triggers did not settle in 150 seconds: deliveries={deliveries:?}, runs={rows}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn assertions(
    run: &Run,
    project: &tokio_postgres::Client,
    tenant: &str,
    digest: &str,
    reader: &mut Child,
) -> anyhow::Result<Value> {
    let principal = wamn_project_state::workflow_user_id().to_string();
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        ensure!(
            reader.try_wait()?.is_none(),
            "CDC reader exited; read cdc-reader.log"
        );
        let rows: Value = project.query_one(
            "SELECT jsonb_build_object(\
            'runs', (SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY created_at), '[]') FROM wamn_run.runs r WHERE tenant_id=$1 AND package_id='rest_connector' AND wiring_id='rest_item'),\
            'items', (SELECT COALESCE(jsonb_agg(to_jsonb(i) ORDER BY key), '[]') FROM rest_consumer_fixture.item i),\
            'last_synced_at', (SELECT value FROM wamn_integration.setting WHERE key='last_synced_at'),\
            'logs', (SELECT COALESCE(jsonb_agg(to_jsonb(l) ORDER BY record_id), '[]') FROM wamn_integration.log l))", &[&tenant],
        ).await?.get(0);
        let runs = rows["runs"].as_array().context("schedule runs")?;
        ensure!(
            runs.iter().all(|r| !matches!(
                r["status"].as_str(),
                Some("failed" | "infrastructure-failure" | "effect-uncertain")
            )),
            "integration schedule failed: {rows}"
        );
        let completed: Vec<_> = runs.iter().filter(|r| r["status"] == "completed").collect();
        if !completed.is_empty() && rows["logs"].as_array().context("log rows")?.len() >= 5 {
            ensure!(
                runs.len() == 1 && completed.len() == 1,
                "expected one completed pull: {rows}"
            );
            let scheduled = completed[0];
            let due = &scheduled["input_json"]["due"];
            ensure!(
                scheduled["trigger_source"] == "schedule"
                    && scheduled["invoked_by"] == "schedule"
                    && scheduled["execution_principal_id"] == principal
                    && scheduled["execution_role"] == PULL_ROLE
                    && scheduled["manifest_digest"] == digest
                    && scheduled["idempotency_key"]
                        == format!(
                            "schedule:rest_connector/rest_item:{}",
                            due.as_str().context("schedule due")?
                        )
                    && rows["last_synced_at"] == *due,
                "wrong schedule identity or watermark: {rows}"
            );
            let expected = json!([{"key":"five","name":"Fifth"},{"key":"four","name":"Fourth"},{"key":"one","name":"First"},{"key":"three","name":"Third"},{"key":"two","name":"Second"}]);
            ensure!(rows["items"] == expected, "consumer rows differ: {rows}");
            let logs = rows["logs"].as_array().unwrap();
            ensure!(
                logs.len() == 5
                    && logs.iter().zip(expected.as_array().unwrap()).all(
                        |(log, item)| log["record_id"] == item["key"]
                            && log["outcome"] == "pushed"
                            && log["delivery_id"].as_str().is_some_and(|id| !id.is_empty())
                    ),
                "logs must contain one pushed row per key: {rows}"
            );
            let requests =
                http_connection::request_records(&http_connection::request_log(run).await?)?;
            let pages: Vec<_> = requests.iter().filter(|r| !r["page"].is_null()).collect();
            ensure!(
                pages.iter().map(|r| r["page"].clone()).collect::<Vec<_>>()
                    == vec![json!(1), json!(2), json!(1), json!(2), json!(3)]
                    && pages
                        .iter()
                        .filter(|r| r["status"] == "429 Too Many Requests")
                        .count()
                        == 1,
                "wrong retry sequence: {pages:?}"
            );
            // All event runs must finish before checking that delivery wrote once.
            let pushed: Value = project.query_one("SELECT COALESCE(jsonb_agg(jsonb_build_object('status', status, 'trigger_source', trigger_source)), '[]') FROM wamn_run.runs WHERE tenant_id=$1 AND wiring_id='rest_item_pushed'", &[&tenant]).await?.get(0);
            let pushed = pushed.as_array().context("push runs")?;
            if pushed.len() == 5 && pushed.iter().all(|r| r["status"] == "completed") {
                return Ok(json!({"database":rows,"pages":pages,"principal":principal}));
            }
        }
        ensure!(
            Instant::now() < deadline,
            "integration did not complete in 150 seconds: {rows}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
