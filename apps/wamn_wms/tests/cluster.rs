//! WMS cluster cases call the product libraries and retain their own assertions.

mod application;
mod bootstrap;
mod build;
mod delivery_case;
mod demo;
mod deployment;
mod reader;
mod session;
mod startup;

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use futures_util::FutureExt as _;
use serde_json::json;
use tokio::process::Command;
use wamn_control_provision::events::{advisory_stream_config, source_stream_config};
use wamn_control_registry::Triple;
use wamn_gate_harness::journey::JourneyDocument;
use wamn_test_infrastructure::event_broker::EventBroker;
use wamn_test_infrastructure::rendering::{EventIdentity, MaterializerInput, render_materializer};
use wamn_test_infrastructure::scratch::ScratchRoot;
use wamn_test_infrastructure::{event_broker, platform, workload};

use crate::wms_runtime_live::write_result;
use deployment::{checked, kubectl};

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn released_wms_routes() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    run_case(Case::Routes).await
}

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn owned_release_delivery() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    run_case(Case::Delivery).await
}

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn released_wms_routes_retain_committed_work_after_label_failure() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    run_case(Case::PartialCompletion).await
}

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl, python3"]
async fn generated_wms_terminal_reports_inventory_success() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "docker", "kind", "kubectl", "helm", "jq", "curl", "python3",
    ]);
    run_case(Case::GeneratedTerminal).await
}

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn restarted_wms_host_retains_compiled_code_and_serves_requests() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    run_case(Case::Startup).await
}

#[derive(Clone, Copy, Debug)]
enum Case {
    Routes,
    Delivery,
    PartialCompletion,
    GeneratedTerminal,
    Startup,
}

async fn run_case(case: Case) -> anyhow::Result<()> {
    let candidate = crate::delivery::candidate()?;
    ensure!(
        candidate.is_none()
            || matches!(case, Case::Routes | Case::PartialCompletion | Case::Startup),
        "supplied-artifact execution supports released routes, partial completion, and restart cases"
    );
    let partial_completion = matches!(case, Case::PartialCompletion);
    let generated_terminal = matches!(case, Case::GeneratedTerminal);
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
    source_clean(&repository).await?;
    ensure!(
        std::env::consts::ARCH == "x86_64",
        "the debug host image requires an x86_64 build host"
    );
    let head = String::from_utf8(
        checked(Command::new("git").current_dir(&repository).args([
            "rev-parse",
            "--verify",
            "HEAD",
        ]))
        .await?,
    )?
    .trim()
    .to_owned();
    let identifier = uuid::Uuid::new_v4().simple().to_string();
    let cluster = format!("wamn-wms-{identifier}");
    let tag = if matches!(case, Case::Delivery) {
        cluster.clone()
    } else {
        format!("wms-{}-{}-debug", &head[..12], &identifier[..12])
    };
    let image = candidate
        .as_ref()
        .map(|(candidate, _)| crate::delivery::image_reference(&candidate.host_image))
        .transpose()?
        .unwrap_or_else(|| format!("wamn-host:{tag}"));
    let lifecycle = repository.join("tools/wms-cluster-journey-run");
    deployment::preflight(&lifecycle, &cluster, &image).await?;
    let target = candidate
        .as_ref()
        .map(|(candidate, _)| candidate.target_directory.clone())
        .or_else(|| std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from))
        .unwrap_or_else(|| repository.join("target"));
    let target = if target.is_absolute() {
        target
    } else {
        repository.join(target)
    };
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
    let delivery_cancelled = pg_walstream::CancellationToken::new();
    let mut run = Box::pin(
        std::panic::AssertUnwindSafe(async {
            build::build(
                &repository,
                &target,
                &evidence,
                generated_terminal,
                matches!(case, Case::Delivery),
            )
            .await?;
            let files = bootstrap::prepare(&repository, work.path())?;
            let scope = Triple::new(
                crate::environment::ORG,
                crate::environment::PROJECT,
                crate::environment::ENVIRONMENT,
            );
            let source = source_stream_config(&scope, 1, Duration::from_secs(120));
            let advisory = advisory_stream_config(&scope, source.num_replicas);
            let broker = event_broker::prepare(
                work.path(),
                &scope,
                crate::environment::TENANT,
                &source,
                &advisory,
                &[],
            )?;
            if matches!(case, Case::Delivery) {
                checked(
                    Command::new(repository.join("tools/delivery-owned"))
                        .arg("build-host")
                        .arg(&cluster)
                        .arg(work.path())
                        .arg(&head),
                )
                .await?;
            } else if candidate.is_none() {
                deployment::prepare_image(&target, work.path())?;
                checked(
                    Command::new(&lifecycle)
                        .arg("build-images")
                        .arg(&cluster)
                        .arg(work.path())
                        .arg(&image)
                        .arg(&head),
                )
                .await?;
            }
            if let Some((candidate, _)) = &candidate {
                crate::delivery::registry_files(candidate, work.path())?;
            }
            checked(
                Command::new(&lifecycle)
                    .arg("create")
                    .arg(&cluster)
                    .arg(work.path())
                    .arg(&repository)
                    .arg(&image),
            )
            .await?;
            let digest = workload::image_ready(
                &lifecycle,
                &cluster,
                work.path(),
                &image,
                &head,
                if candidate.is_some() || matches!(case, Case::Delivery) {
                    "release"
                } else {
                    "debug"
                },
                &evidence,
            )
            .await?;
            checked(kubectl(&cluster, work.path()).args(["create", "namespace", &cluster])).await?;
            let result = Box::pin(run_created(
                &CaseContext {
                    repository: &repository,
                    lifecycle: &lifecycle,
                    cluster: &cluster,
                    work: work.path(),
                    target: &target,
                    evidence: &evidence,
                    image: &image,
                    tag: &tag,
                    source_head: &head,
                    runtime_digest: &digest,
                    files: &files,
                    broker: &broker,
                    source: &source,
                },
                &scope,
                case,
                browser,
                &delivery_cancelled,
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
        Err(failure) => {
            delivery_cancelled.cancel();
            if matches!(case, Case::Delivery) {
                let _ = tokio::time::timeout(Duration::from_secs(180), &mut run).await;
            }
            Ok(Err(anyhow::anyhow!(failure)))
        }
    };
    drop(run);
    let run = observed;
    let failure_capture = if matches!(&run, Ok(Ok(()))) {
        Ok(())
    } else {
        deployment::capture_failure(&cluster, work.path(), &evidence).await
    };
    let removed = checked(
        Command::new(&lifecycle)
            .arg("remove")
            .arg(&cluster)
            .arg(work.path())
            .args(candidate.is_none().then_some(&image)),
    )
    .await;
    let absent = deployment::preflight(&lifecycle, &cluster, &image).await;
    let source = async {
        source_clean(&repository).await?;
        let current = checked(
            Command::new("git")
                .current_dir(&repository)
                .args(["rev-parse", "HEAD"]),
        )
        .await?;
        ensure!(
            std::str::from_utf8(&current)?.trim() == head,
            "the WMS source commit changed during the test"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let work_path = work.path().to_path_buf();
    drop(work);
    let private_files_removed = !work_path.exists();
    write_result(
        &evidence,
        "result.json",
        &json!({
            "source":head,"cluster":cluster,"completed":matches!(&run, Ok(Ok(()))),
            "partial_completion":partial_completion,"generated_terminal":generated_terminal,
            "startup":matches!(case, Case::Startup),"demo":browser,
            "delivery":matches!(case, Case::Delivery),
            "failure_capture":failure_capture.as_ref().err().map(|error|format!("{error:#}")),
            "failure":match &run { Ok(Err(error)) => Some(format!("{error:#}")), Err(_) => Some("test panicked".to_owned()), _ => None },
            "resource_cleanup":removed.is_ok() && absent.is_ok(),
            "private_files_removed":private_files_removed,"source_unchanged":source.is_ok(),
        }),
    )?;
    let cleanup = async {
        removed?;
        absent?;
        source?;
        ensure!(
            private_files_removed,
            "the owned private WMS directory remains after cleanup"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match run {
        Ok(result) => match (result, cleanup) {
            (Ok(()), Ok(())) => {
                if let Some((candidate, manifest)) = &candidate {
                    wamn_control::delivery::report_candidate_success(candidate, manifest)?;
                }
                Ok(())
            }
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
    target: &'a Path,
    evidence: &'a Path,
    image: &'a str,
    tag: &'a str,
    source_head: &'a str,
    runtime_digest: &'a str,
    files: &'a bootstrap::BootstrapFiles,
    broker: &'a EventBroker,
    source: &'a async_nats::jetstream::stream::Config,
}

async fn run_created(
    case_context: &CaseContext<'_>,
    scope: &Triple,
    case: Case,
    browser: bool,
    delivery_cancelled: &pg_walstream::CancellationToken,
) -> anyhow::Result<()> {
    let CaseContext {
        repository,
        lifecycle,
        cluster,
        work,
        target,
        evidence,
        image,
        tag,
        source_head: _,
        runtime_digest,
        files,
        broker,
        source,
    } = *case_context;
    let partial_completion = matches!(case, Case::PartialCompletion);
    let generated_terminal = matches!(case, Case::GeneratedTerminal);
    let measure_startup = matches!(case, Case::Startup);
    let postgres = deployment::inspect(lifecycle, &format!("{cluster}-postgres")).await?;
    let postgres_ip = bootstrap::kind_address(&postgres)?.to_string();
    let postgres_port = bootstrap::postgres_host_port(&postgres)?;
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
    let mut document = JourneyDocument {
        system_pg_url: format!("postgresql://postgres:probe@{postgres_ip}:5432/wamn_system"),
        component_directory: target.join("virtualized/std-empty-environment"),
        compilation_cache_directory: cache,
        flow_http_wasm: target.join("wasm32-wasip2/release/http_route.wasm"),
        component_artifact_base: format!("{registry_authority}/wamn/components"),
        release_artifact_base: format!("{registry_authority}/wamn/releases"),
        route_host: "wms.localhost".into(),
        registry_auth_file: registry_auth,
        host_secret_directory: host_secrets,
        host_secret_namespace: cluster.to_owned(),
        route_caller_secret_output: work.join("route-caller-pat.json"),
        fresh_only_packages: None,
        overlay_compatibility: None,
        postcommit: None,
        materializer: None,
        runtime: None,
    };
    let (route, release) = application::prepare_application(
        &document,
        work,
        &admin_url,
        &application::PublicationInputs {
            scenario_worker: &target.join("debug/wamn-scenario-worker"),
            label_render: &target.join("wasm32-wasip2/release/label_render.wasm"),
            minio_endpoint: &minio_endpoint,
            mint_only: matches!(case, Case::Delivery),
            include_labels: matches!(case, Case::PartialCompletion | Case::Delivery),
        },
        evidence,
    )
    .await?;
    event_broker::write_binding(broker, &nats_url, source)?;
    if measure_startup || matches!(case, Case::Delivery) {
        let provisioning = event_broker::connect(&broker.provisioning, &nats_url).await?;
        wamn_control::event_streams::provision(
            &async_nats::jetstream::new(provisioning.clone()),
            scope,
            source.num_replicas,
            source.duplicate_window,
            &[],
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
            "consumers":[],
        }),
    )?;
    drop(source_observed);
    drop(advisory_observed);
    drop(context);
    observer
        .drain()
        .await
        .context("close the scoped stream observation client")?;
    if matches!(case, Case::Delivery) {
        return delivery_case::run(
            case_context,
            &document,
            &route,
            &release,
            &postgres_ip,
            &nats_url,
            delivery_cancelled,
        )
        .await;
    }
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
                "watchNamespaces":[cluster],"hostNamespaces":[cluster],"allowSharedHosts":false,
            }}))?,
        )?;
        platform::install(
            repository,
            lifecycle,
            cluster,
            work,
            cluster,
            &operator_values,
        )
        .await?;
        let session = session::prepare(case_context, &document).await?;
        let (base, overlay) = application::render_host(
            &document,
            &application::HostBinding {
                host_tag: tag,
                nats_url: &nats_url,
                database_host: &postgres_ip,
                manifest_digest: release.manifest_digest.as_str(),
                source,
                replicas: if measure_startup { 0 } else { 3 },
            },
            work,
        )?;
        session::configure_host(&overlay, &session)?;
        checked(
            Command::new(lifecycle)
                .arg("install-host")
                .arg(cluster)
                .arg(work)
                .arg(cluster)
                .arg(&base)
                .arg(&overlay),
        )
        .await?;
        let cold = if measure_startup {
            Some(startup::cold_host(cluster, work, evidence).await?)
        } else {
            None
        };
        let hosts = workload::hosts_ready(&workload::HostsReadyInput {
            lifecycle,
            cluster,
            work,
            namespace: cluster,
            image,
            runtime_digest,
            replicas: if measure_startup { 1 } else { 3 },
            evidence,
        })
        .await?;
        let flow_http = deployment::publish_runtime(
            repository,
            work,
            &document.flow_http_wasm,
            &format!("{registry_authority}/wamn/flow-http:{tag}"),
            &evidence.join("flow-http-push.json"),
        )
        .await?;
        let materializer = deployment::publish_runtime(
            repository,
            work,
            &target.join("wasm32-wasip2/release/materializer.wasm"),
            &format!("{registry_authority}/wamn/materializer:{tag}"),
            &evidence.join("materializer-push.json"),
        )
        .await?;
        let http_path = application::render_workload(&document, &flow_http, work)?;
        checked(kubectl(cluster, work).args(["apply", "-f"]).arg(&http_path)).await?;
        let http = workload::http_ready(cluster, work, cluster, &hosts, evidence).await?;
        if let Some(cold) = &cold {
            return startup::requests(cluster, work, &document, cold, evidence).await;
        }
        workload::unknown_route(cluster, work, cluster, &document.route_host, evidence).await?;
        let endpoint =
            deployment::expose_route(cluster, work, cluster, lifecycle, &document.route_host)
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

        if partial_completion {
            application::composed_routes(&document, &store, evidence).await?;
        } else {
            application::released_routes(&document, project.as_ref(), evidence).await?;
        }
        let database: tokio_postgres::Config = route.database_url.parse()?;
        let database_name = database
            .get_dbname()
            .context("the project URL names its database")?;
        let loopback_url =
            format!("postgresql://postgres:probe@127.0.0.1:{postgres_port}/{database_name}");
        let instance = http["metadata"]["uid"]
            .as_str()
            .context("the HTTP workload has a UID")?;
        if generated_terminal {
            application::generated_terminal(
                &document,
                &application::TerminalPaths {
                    repository,
                    target,
                    work,
                    evidence,
                },
                &loopback_url,
                instance,
                "success",
            )
            .await?;
        }
        if partial_completion {
            checked(
                Command::new(lifecycle)
                    .arg("remove-labels")
                    .arg(cluster)
                    .arg(work),
            )
            .await?;
            let result = async {
                application::partial_completion(&mut document, project.as_ref(), evidence).await
            }
            .await;
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
            checked(kubectl(cluster, work).args([
                "-n",
                cluster,
                "delete",
                "service",
                "flow-http-nodeport",
            ]))
            .await?;
            checked(kubectl(cluster, work).args([
                "-n",
                cluster,
                "delete",
                "endpointslice",
                "flow-http-nodeport",
            ]))
            .await?;
        }
        let materializer_input = MaterializerInput {
            workload: format!("{}-materializer", crate::environment::PROJECT),
            namespace: cluster.to_owned(),
            image: materializer,
            tenant: crate::environment::TENANT.to_owned(),
            event: EventIdentity {
                org: scope.org.clone(),
                project: scope.project.clone(),
                environment: scope.env.as_str().to_owned(),
            },
            event_stream: source.name.clone(),
            fetch_ms: 500,
            sweep_ms: 500,
        };
        let materializer_path = work.join("materializer.yaml");
        fs::write(
            &materializer_path,
            render_materializer(
                &fs::read_to_string(repository.join("deploy/platform/materializer.example.yaml"))?,
                &materializer_input,
            )?,
        )?;
        checked(
            kubectl(cluster, work)
                .args(["apply", "-f"])
                .arg(&materializer_path),
        )
        .await?;
        workload::materializer_ready(cluster, work, &materializer_input, &hosts, evidence).await?;
        workload::materializer_idle(cluster, work, cluster, &hosts, evidence).await?;
        workload::cross_environment_refused(cluster, work, cluster, &http, evidence).await
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
        "metadata":{"name":"wamn-event-nats","namespace":cluster},"type":"Opaque",
        "stringData":{"username":broker.runtime.username,"password":runtime_password,
            "org":crate::environment::ORG,"project":crate::environment::PROJECT,"environment":crate::environment::ENVIRONMENT,
            "stream_replicas":source.num_replicas.to_string(),"dup_window_secs":source.duplicate_window.as_secs().to_string()}
    })).await?;
    let binding = fs::read_to_string(&broker.binding)?;
    deployment::apply_secret(
        cluster,
        work,
        &json!({"apiVersion":"v1","kind":"Secret",
            "metadata":{"name":"wamn-materializer-nats","namespace":cluster},"type":"Opaque",
            "stringData":{"binding.json":binding}
        }),
    )
    .await
}

async fn source_clean(repository: &Path) -> anyhow::Result<()> {
    let status = checked(Command::new("git").current_dir(repository).args([
        "status",
        "--porcelain",
        "--untracked-files=normal",
        "--",
        ".",
        ":(exclude).beads/issues.jsonl",
        ":(exclude).beads/interactions.jsonl",
    ]))
    .await?;
    ensure!(
        status.is_empty(),
        "the WMS cluster case requires a clean source tree"
    );
    Ok(())
}
