//! WMS cluster cases call the product libraries and retain their own assertions.
//!
//! Each case runs the candidate release that `qualify-release` names: it
//! installs the candidate's packages through the package stage and publishes
//! nothing (docs/plan/platform-deploy.md §8.2).

mod application;
mod bootstrap;
mod build;
mod delivery_case;
mod demo;
mod deployment;
mod reader;
mod startup;

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use futures_util::FutureExt as _;
use serde_json::json;
use tokio::process::Command;
use wamn_control::delivery::Candidate;
use wamn_control::release_chart::{self, ImageSet, ValuesInput};
use wamn_control_provision::events::{advisory_stream_config, source_stream_config};
use wamn_control_registry::Triple;
use wamn_gate_harness::case_release::{self, NAMESPACE, Registry};
use wamn_gate_harness::journey::JourneyDocument;
use wamn_test_infrastructure::event_broker::EventBroker;
use wamn_test_infrastructure::release_chart::{Release, hosts_ready, role_placed};
use wamn_test_infrastructure::scratch::ScratchRoot;
use wamn_test_infrastructure::{event_broker, platform, workload};

use crate::wms_runtime_live::write_result;
use deployment::{checked, kubectl};

/// The drain bound of the case environment: the host pods' grace period and
/// `WAMN_DRAIN_BOUND_SECONDS`, as `deploy/platform/values-host-default.yaml`
/// sets them (R20).
const DRAIN_BOUND_SECONDS: u32 = 70;

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, openssl"]
async fn released_wms_routes() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "openssl"]);
    run_case(Case::Routes).await
}

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, openssl"]
async fn released_wms_routes_retain_committed_work_after_label_failure() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "openssl"]);
    run_case(Case::PartialCompletion).await
}

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, openssl"]
async fn restarted_wms_host_retains_compiled_code_and_serves_requests() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "openssl"]);
    run_case(Case::Startup).await
}

#[derive(Clone, Copy, Debug)]
enum Case {
    Routes,
    PartialCompletion,
    Startup,
}

async fn run_case(case: Case) -> anyhow::Result<()> {
    let candidate = crate::delivery::candidate()?;
    let partial_completion = matches!(case, Case::PartialCompletion);
    // A requested hold keeps the released routes case reachable from a browser.
    let browser =
        matches!(case, Case::Routes) && std::env::var_os("WAMN_JOURNEY_HOLD_SECONDS").is_some();
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let app = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("the WMS test crate has an app parent")?;
    let repository = app
        .parent()
        .and_then(Path::parent)
        .context("the WMS app has a repository parent")?
        .canonicalize()?;
    ensure!(
        std::env::consts::ARCH == "x86_64",
        "the amd64 host image requires an x86_64 machine"
    );
    let identifier = uuid::Uuid::new_v4().simple().to_string();
    let cluster = format!("wamn-wms-{identifier}");
    let image = crate::delivery::image_reference(&candidate.images.host)?;
    // The published attachments accept sessions, so every case starts the
    // session issuer from this image, as the Receiving cases do.
    let identity_image = crate::delivery::image_reference(&candidate.qualification.identity)?;
    let lifecycle = repository.join("tools/wms-cluster-journey-run");
    deployment::preflight(&lifecycle, &cluster, &image).await?;
    // WAMN_WMS_EVIDENCE_DIR names an existing parent directory. The parent is
    // the system temporary directory when the variable is not set.
    let parent =
        std::env::var_os("WAMN_WMS_EVIDENCE_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let evidence = parent
        .canonicalize()
        .with_context(|| {
            format!(
                "the WMS result parent directory {} must exist",
                parent.display()
            )
        })?
        .join(format!("wamn-wms-results-{identifier}"));
    fs::create_dir(&evidence).context("create the new WMS result directory")?;
    println!("WMS test results: {}", evidence.display());
    let work_path = std::env::temp_dir().join(&cluster);
    DirBuilder::new()
        .mode(0o700)
        .create(&work_path)
        .context("create the private owned WMS directory")?;
    let work = ScratchRoot(work_path);
    let mut run = Box::pin(
        std::panic::AssertUnwindSafe(async {
            // The WMS package of the candidate, from its artifact.
            let packages = case_release::open_packages(&candidate).await?;
            let [package] = packages.as_slice() else {
                anyhow::bail!("the WMS candidate names one package");
            };
            let consumers = crate::environment::declared_consumers(
                &crate::environment::package_manifest_at(package.root())?,
            );
            let files = bootstrap::prepare(&repository, work.path())?;
            let scope = Triple::new(
                crate::environment::identity().org.as_str(),
                crate::environment::identity().project.as_str(),
                crate::environment::identity().environment.as_str(),
            );
            let source = source_stream_config(&scope, 1, Duration::from_secs(120));
            let advisory = advisory_stream_config(&scope, source.num_replicas);
            let broker = event_broker::prepare(
                work.path(),
                &scope,
                crate::environment::identity().tenant.as_str(),
                &source,
                &advisory,
                &consumers,
            )?;
            crate::delivery::registry_files(&candidate, work.path(), &[&image, &identity_image])
                .await?;
            checked(
                Command::new(&lifecycle)
                    .arg("create")
                    .arg(&cluster)
                    .arg(work.path())
                    .arg(&repository)
                    .arg(&image)
                    .arg(&identity_image),
            )
            .await?;
            let digest = workload::image_ready(
                &lifecycle,
                &cluster,
                work.path(),
                &image,
                "release",
                &evidence,
            )
            .await?;
            checked(kubectl(&cluster, work.path()).args(["create", "namespace", &cluster])).await?;
            // The operator release, every environment release and its hosts
            // live in the operator namespace (R6).
            checked(kubectl(&cluster, work.path()).args(["create", "namespace", NAMESPACE]))
                .await?;
            let result = Box::pin(run_created(
                &CaseContext {
                    repository: &repository,
                    lifecycle: &lifecycle,
                    cluster: &cluster,
                    work: work.path(),
                    evidence: &evidence,
                    image: &image,
                    identity_image: &identity_image,
                    runtime_digest: &digest,
                    files: &files,
                    broker: &broker,
                    source: &source,
                    consumers: &consumers,
                    packages: &packages,
                    candidate: &candidate,
                },
                &scope,
                case,
                browser,
            ))
            .await;
            demo::hold(work.path(), browser && result.is_ok()).await?;
            result
        })
        .catch_unwind(),
    );
    let observed = tokio::select! {
        result = &mut run => Ok(result),
        _ = interrupt.recv() => Err("the WMS test received SIGINT"),
        _ = terminate.recv() => Err("the WMS test received SIGTERM"),
        _ = hangup.recv() => Err("the WMS test received SIGHUP"),
    };
    let observed = match observed {
        Ok(result) => result,
        Err(failure) => Ok(Err(anyhow::anyhow!(failure))),
    };
    drop(run);
    let run = observed;
    let failure_capture = if matches!(&run, Ok(Ok(()))) {
        Ok(())
    } else {
        let identity = crate::environment::identity();
        match release_chart::release_name(
            identity.org.as_str(),
            identity.project.as_str(),
            identity.environment.as_str(),
        ) {
            Ok(name) => deployment::capture_failure(&cluster, work.path(), &name, &evidence).await,
            Err(error) => Err(error),
        }
    };
    let removed = checked(
        Command::new(&lifecycle)
            .arg("remove")
            .arg(&cluster)
            .arg(work.path()),
    )
    .await;
    let absent = deployment::preflight(&lifecycle, &cluster, &image).await;
    let work_path = work.path().to_path_buf();
    drop(work);
    let private_files_removed = !work_path.exists();
    write_result(
        &evidence,
        "result.json",
        &json!({
            "cluster":cluster,"completed":matches!(&run, Ok(Ok(()))),
            "partial_completion":partial_completion,
            "startup":matches!(case, Case::Startup),"demo":browser,
            "failure_capture":failure_capture.as_ref().err().map(|error|format!("{error:#}")),
            "failure":match &run { Ok(Err(error)) => Some(format!("{error:#}")), Err(_) => Some("test panicked".to_owned()), _ => None },
            "resource_cleanup":removed.is_ok() && absent.is_ok(),
            "private_files_removed":private_files_removed,
        }),
    )?;
    let cleanup = async {
        removed?;
        absent?;
        ensure!(
            private_files_removed,
            "the owned private WMS directory remains after cleanup"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match run {
        Ok(result) => match (result, cleanup) {
            (Ok(()), Ok(())) => wamn_control::delivery::report_candidate_success(&candidate),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => {
                Err(error.context(format!("WMS cleanup also failed: {cleanup:#}")))
            }
        },
        Err(panic) => {
            if let Err(error) = cleanup {
                eprintln!("WMS cleanup failed after a test panic: {error:#}");
            }
            std::panic::resume_unwind(panic)
        }
    }
}

/// What one cluster case is built from and where it writes. Every case carries
/// the same set, so it travels as one argument.
struct CaseContext<'a> {
    repository: &'a Path,
    lifecycle: &'a Path,
    cluster: &'a str,
    work: &'a Path,
    evidence: &'a Path,
    image: &'a str,
    identity_image: &'a str,
    runtime_digest: &'a str,
    files: &'a bootstrap::BootstrapFiles,
    broker: &'a EventBroker,
    source: &'a async_nats::jetstream::stream::Config,
    /// The materializer consumers of the candidate's WMS package.
    consumers: &'a [async_nats::jetstream::consumer::pull::Config],
    /// The candidate's packages, from their artifacts.
    packages: &'a [wamn_control::package_artifact::OpenedPackage],
    /// The release the case installs and deploys.
    candidate: &'a Candidate,
}

/// Start the session issuer that the published WMS attachments need, and
/// return the issuer and the instance suffix the host trusts.
async fn start_session_issuer(
    context: &CaseContext<'_>,
    document: &JourneyDocument,
    manifest_digest: &str,
) -> anyhow::Result<(String, String)> {
    let identity = crate::environment::identity();
    wamn_gate_harness::session_issuer::prepare_application(
        &wamn_gate_harness::session_issuer::IssuerCluster {
            name: context.cluster,
            namespace: context.cluster,
            work: context.work,
            evidence: context.evidence,
            repository: context.repository,
            lifecycle: context.lifecycle,
            system_database_url: &document.system_pg_url,
            identity_image: Some(context.identity_image),
            org: identity.org.as_str(),
            project: identity.project.as_str(),
            environment: identity.environment.as_str(),
            tenant: identity.tenant.as_str(),
        },
        manifest_digest,
    )
    .await
}

async fn run_created(
    case_context: &CaseContext<'_>,
    scope: &Triple,
    case: Case,
    browser: bool,
) -> anyhow::Result<()> {
    let CaseContext {
        repository,
        lifecycle,
        cluster,
        work,
        evidence,
        image,
        identity_image: _,
        runtime_digest,
        files,
        broker,
        source,
        consumers,
        packages,
        candidate,
    } = *case_context;
    let (manifest, manifest_digest) = candidate.manifest()?;
    let partial_completion = matches!(case, Case::PartialCompletion);
    let measure_startup = matches!(case, Case::Startup);
    let postgres = deployment::inspect(lifecycle, &format!("{cluster}-postgres")).await?;
    let postgres_ip = bootstrap::kind_address(&postgres)?.to_string();
    let registry = deployment::inspect(lifecycle, &format!("{cluster}-registry")).await?;
    let registry_authority = format!("{}:5000", bootstrap::kind_address(&registry)?);
    let nats = deployment::inspect(lifecycle, &format!("{cluster}-nats")).await?;
    let nats_url = format!("nats://{}:4222", bootstrap::kind_address(&nats)?);
    let minio = deployment::inspect(lifecycle, &format!("{cluster}-minio")).await?;
    let minio_endpoint = format!("http://{}:9000", bootstrap::kind_address(&minio)?);
    let registry_auth = bootstrap::write_registry_auth(files, work, &registry_authority)?;
    let admin_url = format!("postgresql://postgres:probe@{postgres_ip}:5432/postgres");
    deployment::wait_postgres(&admin_url).await?;
    deployment::wait_registry(&registry_authority, &registry_auth).await?;
    deployment::wait_minio(&minio_endpoint).await?;
    checked(
        Command::new(lifecycle)
            .arg("install-labels")
            .arg(cluster)
            .arg(work),
    )
    .await?;
    let host_secrets = work.join("host-secrets");
    let cache = work.join("wasmtime-cache");
    DirBuilder::new().mode(0o700).create(&host_secrets)?;
    DirBuilder::new().mode(0o700).create(&cache)?;
    // The case reads no built component: the hosts pull the candidate's.
    let mut document = JourneyDocument {
        system_pg_url: format!("postgresql://postgres:probe@{postgres_ip}:5432/wamn_system"),
        component_directory: PathBuf::new(),
        compilation_cache_directory: cache,
        flow_http_wasm: PathBuf::new(),
        component_artifact_base: format!("{registry_authority}/wamn/components"),
        release_artifact_base: format!("{registry_authority}/wamn/releases"),
        route_host: crate::environment::identity().route_host.clone(),
        registry_auth_file: registry_auth,
        host_secret_directory: host_secrets,
        host_secret_namespace: NAMESPACE.to_owned(),
        operator_secret_output: work.join("operator-pat.json"),
        postcommit: None,
        materializer: None,
        runtime: None,
    };
    // The candidate's release, its components and its role images, copied by
    // digest into this registry, which the hosts read (R12, R15).
    let registry = Registry {
        authority: registry_authority.clone(),
        auth_file: Some(document.registry_auth_file.clone()),
        insecure: true,
        ca_paths: Vec::new(),
    };
    case_release::copy_release(candidate, &registry).await?;
    let (route, _) = application::provision(&document, work, &admin_url).await?;
    crate::environment::install(candidate, &document, &route, packages, &minio_endpoint).await?;
    application::seed(&route, evidence).await?;
    event_broker::write_binding(broker, &nats_url, source)?;
    if measure_startup {
        let provisioning = event_broker::connect(&broker.provisioning, &nats_url).await?;
        wamn_control::event_streams::provision(
            &async_nats::jetstream::new(provisioning.clone()),
            scope,
            source.num_replicas,
            source.duplicate_window,
            consumers,
        )
        .await?;
        provisioning.drain().await?;
    } else {
        crate::environment::configure_cdc(
            &document,
            &route,
            work,
            &bootstrap::random_password()?,
            &nats_url,
            &broker.provisioning,
            source,
            consumers,
        )
        .await?;
    }
    let observer = event_broker::connect(&broker.observer, &nats_url).await?;
    let context = async_nats::jetstream::new(observer.clone());
    let source_observed = context.get_stream(&source.name).await?;
    let advisory = advisory_stream_config(scope, source.num_replicas);
    let advisory_observed = context.get_stream(&advisory.name).await?;
    write_result(
        evidence,
        "event-streams.json",
        &json!({
            "source":source_observed.cached_info().config,
            "advisory":advisory_observed.cached_info().config,
            "consumers":consumers,
        }),
    )?;
    drop(source_observed);
    drop(advisory_observed);
    drop(context);
    observer
        .drain()
        .await
        .context("close the scoped stream observation client")?;
    let (project, project_task) =
        wamn_control::dev::environment::connect(&route.database_url).await?;
    let reader_args = if measure_startup {
        None
    } else {
        Some(reader::args(
            work,
            &nats_url,
            source,
            &broker.publisher.username,
            &broker.publisher.password_file,
        )?)
    };
    let cdc_url = reader_args.as_ref().map(|args| args.cdc_url.clone());
    let cancellation = wamn_cdc_reader::ReaderShutdown::new();
    let _cancel_on_drop = cancellation.clone().into_guard();
    let reader = async {
        match reader_args {
            Some(args) => wamn_cdc_reader::run_with_shutdown(args, cancellation.clone()).await,
            None => std::future::pending::<anyhow::Result<()>>().await,
        }
    };
    tokio::pin!(reader);
    let assertions = async {
        if let Some(cdc_url) = &cdc_url {
            reader::wait_streaming(project.as_ref(), cdc_url, evidence).await?;
        }

        deployment::install_application_secrets(&document, files, cluster, work, &postgres_ip)
            .await?;
        install_event_secrets(cluster, work, broker, source).await?;
        let operator_values = work.join("operator-values.json");
        fs::write(
            &operator_values,
            serde_json::to_vec_pretty(&json!({"operator":{
                "watchNamespaces":[NAMESPACE],"hostNamespaces":[NAMESPACE],"allowSharedHosts":false,
            }}))?,
        )?;
        platform::install(
            repository,
            lifecycle,
            cluster,
            work,
            NAMESPACE,
            &operator_values,
        )
        .await?;
        let (issuer, instance) =
            start_session_issuer(case_context, &document, manifest_digest.as_str()).await?;
        let replicas = if measure_startup { 1 } else { 3 };
        let (_, overlay) = application::render_host(
            &document,
            &application::HostBinding {
                host_tag: cluster,
                nats_url: &nats_url,
                database_host: &postgres_ip,
                manifest_digest: manifest_digest.as_str(),
                source,
                replicas,
                session: (&issuer, &instance),
                candidate: Some(candidate),
            },
            work,
        )?;
        let (http, materializer) = case_release::copy_roles(candidate, &registry).await?;
        let host_image = if image.contains('@') {
            image.to_owned()
        } else {
            format!("{image}@{runtime_digest}")
        };
        let identity = crate::environment::identity();
        let (org, project_name, env) = (
            identity.org.as_str(),
            identity.project.as_str(),
            identity.environment.as_str(),
        );
        // The environment release installs the host group and the roles the
        // release implies, as `env apply` does (§9.2).
        let (name, label) = case_release::install(
            &case_release::Install { cluster, work },
            &ImageSet {
                host: host_image.clone(),
                http,
                materializer,
            },
            &ValuesInput {
                org: org.to_owned(),
                project: project_name.to_owned(),
                env: env.to_owned(),
                manifest_digest: manifest_digest.to_string(),
                artifact_base: document.release_artifact_base.clone(),
                route_host: document.route_host.clone(),
                applications: [(
                    "wamn_wms".to_owned(),
                    wamn_catalog::ApplicationDefinition {
                        packages: manifest
                            .release
                            .packages
                            .iter()
                            .map(|package| package.package_id().to_owned())
                            .collect(),
                    },
                )]
                .into(),
                roles: release_chart::roles(&manifest, org, project_name, env),
                drain_bound_seconds: DRAIN_BOUND_SECONDS,
                actor: "wms cluster case".to_owned(),
                host_group: case_release::host_group(&fs::read_to_string(&overlay)?)?,
            },
        )
        .await?;
        let chart = case_release::release_chart_label()?;
        let environment = Release {
            chart: &chart,
            cluster,
            work,
            namespace: NAMESPACE,
            name: &name,
            label: &label,
            digest: manifest_digest.as_str(),
            evidence,
        };
        let service = format!("{name}-http");
        let cold = if measure_startup {
            Some(startup::cold_host(cluster, work, &name, evidence).await?)
        } else {
            None
        };
        let hosts = hosts_ready(&environment, &host_image, runtime_digest, replicas).await?;
        let http = role_placed(&environment, "http", &hosts).await?;
        if let Some(cold) = &cold {
            return startup::requests(cluster, work, &document, cold, evidence).await;
        }
        workload::unknown_route(
            cluster,
            work,
            NAMESPACE,
            &service,
            &document.route_host,
            evidence,
        )
        .await?;
        let endpoint = deployment::expose_route(
            cluster,
            work,
            NAMESPACE,
            &service,
            lifecycle,
            &document.route_host,
        )
        .await?;
        if browser {
            demo::start(
                lifecycle,
                cluster,
                work,
                &endpoint,
                &document.route_host,
                evidence,
            )
            .await?;
        }
        document.runtime = Some(application::runtime_phase(endpoint));
        let store = bootstrap::object_store(files, &minio_endpoint)?;

        // The movement_label workflow runs on the materializer's delivery of
        // the move's row event, so the materializer serves before any move.
        role_placed(&environment, "materializer", &hosts).await?;
        application::released_routes(&document, &store, evidence).await?;
        if partial_completion {
            checked(
                Command::new(lifecycle)
                    .arg("remove-labels")
                    .arg(cluster)
                    .arg(work),
            )
            .await?;
            let result =
                application::partial_completion(&mut document, project.as_ref(), evidence).await;
            let restored = checked(
                Command::new(lifecycle)
                    .arg("install-labels")
                    .arg(cluster)
                    .arg(work),
            )
            .await;
            match (result, restored) {
                (Ok(()), Ok(_)) => {}
                (Err(error), Ok(_)) | (Ok(()), Err(error)) => return Err(error),
                (Err(error), Err(cleanup)) => {
                    return Err(
                        error.context(format!("labels restoration also failed: {cleanup:#}"))
                    );
                }
            }
        }
        if !browser {
            let nodeport = format!("{service}-nodeport");
            for kind in ["service", "endpointslice"] {
                checked(kubectl(cluster, work).args(["-n", NAMESPACE, "delete", kind, &nodeport]))
                    .await?;
            }
        }
        workload::materializer_idle(cluster, work, NAMESPACE, &hosts, evidence).await?;
        workload::cross_environment_refused(cluster, work, NAMESPACE, &http, evidence).await
    };
    let result = tokio::select! {
        ended = &mut reader => match ended {
            Ok(()) => Err(anyhow::anyhow!("the production CDC reader stopped before the WMS assertions completed")),
            Err(error) => Err(error),
        },
        result = assertions => {
            cancellation.shutdown();
            if measure_startup { result } else { match (result, tokio::time::timeout(Duration::from_secs(10), &mut reader).await) {
                (Ok(()), Ok(Ok(()))) => Ok(()),
                (Err(error), _) | (Ok(()), Ok(Err(error))) => Err(error),
                (Ok(()), Err(_)) => Err(anyhow::anyhow!("the production CDC reader did not stop within ten seconds")),
            }}
        }
    };
    drop(project);
    project_task.abort();
    result
}

async fn install_event_secrets(
    cluster: &str,
    work: &Path,
    broker: &EventBroker,
    source: &async_nats::jetstream::stream::Config,
) -> anyhow::Result<()> {
    let runtime_password = fs::read_to_string(&broker.runtime.password_file)?;
    deployment::apply_secret(cluster, work, &json!({"apiVersion":"v1","kind":"Secret",
        "metadata":{"name":"wamn-event-nats","namespace":NAMESPACE},"type":"Opaque",
        "stringData":{"username":broker.runtime.username,"password":runtime_password,
            "org":crate::environment::identity().org.as_str(),"project":crate::environment::identity().project.as_str(),"environment":crate::environment::identity().environment.as_str(),
            "stream_replicas":source.num_replicas.to_string(),"dup_window_secs":source.duplicate_window.as_secs().to_string()}
    })).await?;
    let binding = fs::read_to_string(&broker.binding)?;
    deployment::apply_secret(
        cluster,
        work,
        &json!({"apiVersion":"v1","kind":"Secret",
            "metadata":{"name":"wamn-materializer-nats","namespace":NAMESPACE},"type":"Opaque",
            "stringData":{"binding.json":binding}
        }),
    )
    .await
}
