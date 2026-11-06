//! Platform objects rendered without a cluster.
use serde_json::json;
use wamn_environment_control as control;
#[path = "support/platform_install.rs"]
mod platform_install;
use std::path::Path;
use std::time::Instant;
#[path = "support/release_documents.rs"]
pub mod release_documents;
use release_documents::fixture_environment;

#[test]
fn prekind_platform_renders() -> anyhow::Result<()> {
    use wamn_gate_harness::render_check::{
        assert_complete, credential_secrets, environment, operator_prerequisites, template,
    };
    let started = Instant::now();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let work = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    // The platform publishes built application releases in kind. A fixture H
    // document renders the same HTTP and materializer role shapes without Wasm.
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(
        &release_documents::fixture_releases()?[2],
    )?;
    for project in ["receiving", "wms"] {
        let (_, mut document) = fixture_environment(&repository, project)?;
        document.org = "test".into();
        let path = repository.join(format!("deploy/gcp/host-group-{project}.yaml"));
        let group = wamn_environment_control::platform::render_host_group(
            &std::fs::read_to_string(&path)?,
            &document,
            "registry.invalid/wamn",
            "render",
        )?;
        let path = work.path().join(format!("host-group-{project}.yaml"));
        std::fs::write(&path, serde_yaml::to_string(&group)?)?;
        let mut secrets = credential_secrets(&document.triple(), "hosts");
        secrets.extend(operator_prerequisites("hosts"));
        secrets.extend(wamn_environment_control::platform::render_event_secrets(
            &document,
        )?);
        // cert-manager issues the environment's NATS certificate Secret.
        secrets.push(json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": format!("wamn-nats-test--{project}--dev"), "namespace": "hosts"},
            "data": {"ca.crt": "", "tls.crt": "", "tls.key": ""}
        }));
        assert_complete(
            &template(&environment(&document, &manifest, &path, 70)?, "hosts")?,
            &secrets,
        )?;
        println!(
            "platform {project}: {}",
            path.file_name().unwrap().to_string_lossy()
        );
    }
    println!("prekind platform renders elapsed {:?}", started.elapsed());
    Ok(())
}

#[test]
fn prekind_install_renders() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    assert!(
        render
            .objects
            .iter()
            .map(|(_, object)| object)
            .chain(&render.owner_inputs)
            .all(|object| object["kind"].is_string())
    );
    assert_eq!(render.documents.len(), 2);
    assert_eq!(render.fixtures.releases.len(), 2);
    assert_eq!(render.cluster.org, "test");
    let rows = render
        .objects
        .iter()
        .filter_map(|(_, object)| object["data"]["release.json"].as_str())
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(rows.len(), 3);
    for row in rows {
        assert_eq!(row["webs"], json!({}));
    }
    Ok(())
}

#[cfg(feature = "test-util")]
fn copy_journey_inputs(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_journey_inputs(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(feature = "test-util")]
fn journey_apply(
    repository: &Path,
    render: &platform_install::Render,
) -> anyhow::Result<Vec<serde_json::Value>> {
    Ok(control::platform::render::install(
        &render.cluster,
        &render
            .documents
            .iter()
            .map(|(_, document)| document.clone())
            .collect::<Vec<_>>(),
        repository,
        &render.fixtures,
    )?
    .into_iter()
    .map(|(_, object)| object)
    .collect())
}

#[cfg(feature = "test-util")]
fn operator_cpu(objects: &[serde_json::Value]) -> &serde_json::Value {
    &objects
        .iter()
        .find(|object| {
            object["kind"] == "Deployment" && object["metadata"]["name"] == "runtime-operator"
        })
        .expect("the production operator deployment")["spec"]["template"]["spec"]["containers"][0]["resources"]
        ["requests"]["cpu"]
}

#[cfg(feature = "test-util")]
fn journey_cluster_change(
    project: &str,
    entry: fn(
        &Path,
        &control::platform::journey::ClusterInput<'_>,
    ) -> anyhow::Result<control::platform::journey::Cluster>,
) -> anyhow::Result<()> {
    use control::platform::{install, journey};
    let render = platform_install::build()?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let work = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    copy_journey_inputs(&repository.join("deploy"), &work.path().join("deploy"))?;
    let original: Vec<_> = render
        .objects
        .iter()
        .map(|(_, object)| object.clone())
        .collect();
    assert_ne!(operator_cpu(&original), "137m");
    let archive = work
        .path()
        .join("deploy/platform/release/charts/runtime-operator-2.11.0.tgz");
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(work.path())
        .status()?;
    assert!(status.success(), "unpack the temporary operator chart");
    let values_path = work.path().join("runtime-operator/values.yaml");
    let mut values: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(&values_path)?)?;
    values["operator"]["resources"]["requests"]["cpu"] = "137m".into();
    std::fs::write(values_path, serde_yaml::to_string(&values)?)?;
    let status = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(work.path())
        .arg("runtime-operator")
        .status()?;
    assert!(
        status.success(),
        "pack the changed temporary operator chart"
    );
    let scopes = [wamn_control_registry::Triple::new("test", project, "dev")];
    let bundle = entry(
        work.path(),
        &journey::ClusterInput {
            release_chart: &work.path().join("deploy/platform/release"),
            operator_namespace: "wamn-system",
            host_namespace: "wamn-system",
            org: "test",
            environments: &scopes,
        },
    )?;
    let rendered = install::helm_template(&bundle.operator)?;
    let apply = journey_apply(work.path(), &render)?;
    assert_eq!(
        operator_cpu(&apply),
        "137m",
        "platform apply uses the changed chart"
    );
    assert_eq!(
        operator_cpu(&rendered),
        operator_cpu(&apply),
        "{project} uses the changed production operator chart"
    );
    assert_eq!(
        bundle.operator["values"]["global"]["nats"]["schedulerUrl"],
        "nats://wamn-nats.wamn-system.svc.cluster.local:4222"
    );
    assert!(
        bundle
            .prerequisites
            .iter()
            .any(|object| object["kind"] == "ServiceAccount"
                && object["metadata"]["name"] == "wamn-host"
                && object["metadata"]["namespace"] == "wamn-system")
    );
    if project == "delivery" {
        let database =
            journey::delivery_database(work.path(), "application-database", "application-pg")?;
        assert_eq!(database.objects[1]["metadata"]["name"], "application-pg");
        assert_eq!(
            database.objects[1]["metadata"]["namespace"],
            "application-database"
        );
        assert_eq!(database.fixture_objects[0]["spec"]["type"], "NodePort");
        assert_eq!(
            database.fixture_objects[0]["spec"]["selector"]["cnpg.io/cluster"],
            "application-pg"
        );
        assert!(
            bundle
                .prerequisites
                .iter()
                .any(|object| object["kind"] == "Deployment"
                    && object["metadata"]["name"] == "cnpg-controller-manager")
        );
    } else {
        journey_release_change(project, work.path(), &render)?;
    }
    Ok(())
}

#[cfg(feature = "test-util")]
fn journey_release_change(
    project: &str,
    repository: &Path,
    render: &platform_install::Render,
) -> anyhow::Result<()> {
    use control::platform::{install, journey};
    use wamn_release_control::release_chart;
    let environment = &render
        .documents
        .iter()
        .find(|(_, document)| document.project == project)
        .expect("journey environment")
        .1;
    let key = wamn_control_registry::project_env_key(
        &environment.org,
        &environment.project,
        &environment.env,
    );
    let chart = render.fixtures.directory.join("release-chart");
    let defaults = chart.join("values.yaml");
    let mut values: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(&defaults)?)?;
    values["platform"]["roles"]["http"]["maxConcurrency"] = 7.into();
    std::fs::write(defaults, serde_yaml::to_string(&values)?)?;
    let values = release_chart::ValuesInput {
        org: environment.org.clone(),
        project: project.into(),
        env: environment.env.clone(),
        manifest_digest: environment.release.to_string(),
        artifact_base: "registry.invalid/wamn/releases".into(),
        route_host: environment.route_host.clone(),
        applications: environment.applications.clone(),
        roles: release_chart::roles(
            &render.fixtures.releases[&key],
            &environment.org,
            project,
            &environment.env,
        ),
        drain_bound_seconds: render.fixtures.drain_bound_seconds,
        actor: render.fixtures.actor.clone(),
        host_group: serde_yaml::from_str(
            "{env: [{name: WAMN_COMPONENT_ARTIFACT_BASE, value: registry.invalid/wamn/components}]}",
        )?,
    };
    let bundle = journey::release(&journey::ReleaseInput {
        chart: &chart,
        namespace: "wamn-system",
        values: &values,
        nats_url: "nats://wamn-nats.wamn-system.svc.cluster.local:4222",
        otlp_endpoint: "http://otel-collector.wamn-system.svc.cluster.local:4317",
        insecure_registry: true,
    })?;
    let rendered = install::helm_template(&bundle.chart)?;
    let apply = journey_apply(repository, render)?;
    let name = format!(
        "{}-http",
        release_chart::release_name(&environment.org, project, &environment.env)?
    );
    let concurrency = |objects: &[serde_json::Value]| {
        objects
            .iter()
            .find(|object| {
                object["kind"] == "WorkloadDeployment" && object["metadata"]["name"] == name
            })
            .expect("HTTP workload")["spec"]["template"]["spec"]["components"][0]["maxConcurrency"]
            .clone()
    };
    assert_eq!(
        concurrency(&apply),
        7,
        "platform apply uses the changed release defaults"
    );
    assert_eq!(
        concurrency(&rendered),
        concurrency(&apply),
        "the live release adapter uses the changed defaults"
    );
    assert!(
        bundle
            .objects
            .iter()
            .any(|object| object["kind"] == "ServiceAccount")
    );
    assert_eq!(
        bundle.chart["values"]["applications"],
        serde_json::to_value(&values.applications)?
    );
    Ok(())
}

#[cfg(feature = "test-util")]
#[test]
fn receiving_uses_install_builders() -> anyhow::Result<()> {
    journey_cluster_change("receiving", control::platform::journey::cluster)
}

#[cfg(feature = "test-util")]
#[test]
fn wms_uses_install_builders() -> anyhow::Result<()> {
    journey_cluster_change("wms", control::platform::journey::cluster)
}

#[cfg(feature = "test-util")]
#[test]
fn delivery_uses_install_builders() -> anyhow::Result<()> {
    journey_cluster_change("delivery", control::platform::journey::delivery)
}

#[cfg(feature = "test-util")]
#[test]
fn identity_uses_install_builders() -> anyhow::Result<()> {
    use control::platform::{identity, install, journey};
    let render = platform_install::build()?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let work = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    copy_journey_inputs(&repository.join("deploy"), &work.path().join("deploy"))?;
    let values_path = work.path().join("deploy/platform/identity/values.yaml");
    let mut values: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(&values_path)?)?;
    values["resources"]["requests"]["cpu"] = "139m".into();
    std::fs::write(values_path, serde_yaml::to_string(&values)?)?;
    let apply = journey_apply(work.path(), &render)?;
    let cpu = |objects: &[serde_json::Value], name: &str| {
        objects
            .iter()
            .find(|object| object["kind"] == "Deployment" && object["metadata"]["name"] == name)
            .expect("identity deployment")["spec"]["template"]["spec"]["containers"][0]["resources"]
            ["requests"]["cpu"]
            .clone()
    };
    assert_eq!(cpu(&apply, "identity"), "139m");
    for mode in [
        identity::Mode::Foundation,
        identity::Mode::Session {
            targets: vec!["fixture-target".into()],
            nats: None,
        },
    ] {
        let foundation = matches!(mode, identity::Mode::Foundation);
        let entry = journey::identity(&identity::ChartInput {
            chart: &work.path().join("deploy/platform/identity"),
            name: "fixture-identity",
            namespace: "fixture-identity",
            image: "wamn-identity:fixture",
            pull_policy: "Never",
            issuer: "https://fixture-identity.fixture-identity.svc.cluster.local",
            database_secret: "fixture-db",
            tls_secret: "fixture-tls",
            operator_ca_secret: "",
            resend_secret: "",
            resend_from: "",
            invite_url: "",
            mode,
        })?;
        let rendered = install::helm_template(&entry)?;
        assert_eq!(cpu(&rendered, "fixture-identity"), cpu(&apply, "identity"));
        assert_eq!(entry["values"]["authNats"]["url"], "");
        assert_eq!(
            entry["values"]["sessionTargetSecrets"],
            if foundation {
                json!([])
            } else {
                json!(["fixture-target"])
            }
        );
        let deployment = rendered
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .unwrap();
        let volumes = deployment["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .unwrap();
        let variables = deployment["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array()
            .unwrap();
        assert!(
            variables
                .iter()
                .any(|variable| variable["name"] == "WAMN_IDENTITY_DATABASE_URL"
                    && variable["valueFrom"]["secretKeyRef"]["name"] == "fixture-db")
        );
        assert!(
            volumes
                .iter()
                .any(|volume| volume["secret"]["secretName"] == "fixture-tls")
        );
        assert_eq!(
            volumes
                .iter()
                .any(|volume| volume["secret"]["secretName"] == "fixture-target"),
            !foundation
        );
    }
    Ok(())
}
