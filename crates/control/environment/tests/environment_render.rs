//! Product environment rendering without a cluster or a running host.

use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;
use wamn_control_provision::WorkloadRoleFamily;
use wamn_control_registry::Triple;
use wamn_environment_control::environment::document::EnvironmentDocument;
#[path = "support/release_documents.rs"]
pub mod release_documents;
use release_documents::workflow as integration;
use release_documents::{
    NAMESPACE, ORG, PROJECT, environment_releases as fixture_releases, fixture_document,
    fixture_host_group, rollback_host_group,
};
use wamn_gate_harness::render_check::{
    assert_complete, credential_secrets, environment as render_values, operator_prerequisites,
    prerequisite, template,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

#[test]
fn prekind_deploy_connections_cover_the_wms_environment() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let document =
        EnvironmentDocument::compile(&repository.join("deploy/gcp/environments/dkk--wms--dev.k"))?;
    let directory = repository.join("deploy/gcp/connections");
    for name in document
        .applications
        .values()
        .flat_map(|app| app.connections.values())
    {
        let connection: Value =
            serde_json::from_slice(&std::fs::read(directory.join(format!("{name}.json")))?)?;
        let integration: Value = serde_json::from_slice(&std::fs::read(
            directory.join(format!("{name}.integration.json")),
        )?)?;
        assert_eq!(connection["name"], name.as_str());
        assert_eq!(connection["project"], document.project);
        assert_eq!(connection["env"], document.env);
        assert!(document.integrations.contains_key(name));
        wamn_catalog::RequirementType::for_integration(
            integration["integration_type"]
                .as_str()
                .context("integration type")?,
        )
        .context("supported host integration")?
        .check_definition(&connection["parameters"])
        .map_err(anyhow::Error::msg)?;
        if let Some(system) = integration["external_system"].as_str() {
            let external: Value = serde_json::from_slice(&std::fs::read(
                directory.join(format!("{system}.external-system.json")),
            )?)?;
            assert_eq!(external["name"], system);
            assert_eq!(external["project"], document.project);
        }
    }
    Ok(())
}

fn manifest(bytes: &[u8]) -> anyhow::Result<wamn_catalog::ServingManifest> {
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(bytes)?;
    Ok(manifest)
}

#[test]
fn changing_one_application_selection_leaves_the_other_release_unchanged() -> anyhow::Result<()> {
    use std::collections::BTreeMap;
    use wamn_environment_control::environment::{document::DeclaredRelease, write};

    let work = ScratchRoot::create()?;
    let mut releases = BTreeMap::new();
    let mut applications = serde_json::Map::new();
    for application in ["orders", "receiving"] {
        let mut release: wamn_catalog::ServingManifest = serde_json::from_value(json!({
            "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            "release": {"packages": [{"package-id": application, "package-version":"1.0.0"}]},
            "components": [], "routes": [],
            "attachments": {"http": {"type":"http", "package-id":application,
                "component":application, "operation":"get", "definition":{},
                "definition-hash":wamn_execution_contract::bytes_sha256(application.as_bytes()),
                "auth-policy":{"modes":["none"]}}}
        }))?;
        let member = wamn_catalog::ServingEntryMember {
            package_id: application.into(),
            component: application.into(),
            digest: wamn_execution_contract::bytes_sha256(application.as_bytes()),
        };
        release.composed_entry = Some(wamn_catalog::ServingComposedEntry {
            reference: format!("entries@{}", member.digest),
            route_digest: member.digest.clone(),
            application: member.clone(),
            members: [member].into(),
        });
        applications.insert(application.into(), json!({
            "packages":[application], "current":release.digest().as_str(),
            "hostname":format!("{application}.example"),
            "client":{"package":application, "web":wamn_execution_contract::bytes_sha256(application.as_bytes())}
        }));
        releases.insert(application, release);
    }
    let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org":ORG,"project":PROJECT,"env":"dev","target":"cloud","policy":"dev",
        "applications":applications
    }))?)?;
    let group = work.path().join("host.yaml");
    std::fs::write(
        &group,
        serde_yaml::to_string(&fixture_host_group(&document, "registry.invalid"))?,
    )?;
    let render = |document: &EnvironmentDocument,
                  application: &str,
                  release: &wamn_catalog::ServingManifest|
     -> anyhow::Result<_> {
        let selection = &document.applications[application];
        let (values, _) = write::release_values(
            document,
            application,
            release,
            &selection.current.to_string(),
            "registry.invalid/wamn/releases",
            70,
            "render",
            Some(&group),
        )?;
        assert_eq!(
            values["applications"]
                .as_mapping()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            [&serde_yaml::Value::from(application)]
        );
        assert_eq!(
            values["routeHost"].as_str(),
            Some(selection.hostname.as_str())
        );
        let carrier = template(&values, NAMESPACE)?;
        let row = carrier
            .iter()
            .find(|object| object["data"]["release.json"].is_string())
            .unwrap();
        let name = row["metadata"]["name"].as_str().unwrap();
        let object = carrier
            .iter()
            .find(|object| object["metadata"]["labels"]["wamn.route"] == "true")
            .unwrap();
        let route: Value = serde_json::from_str(object["data"]["route.json"].as_str().unwrap())?;
        assert_eq!(route["host"], selection.hostname);
        assert_eq!(route["release"], name);
        for role in ["http", "files"] {
            let service = carrier
                .iter()
                .find(|object| {
                    object["kind"] == "Service"
                        && object["metadata"]["name"] == format!("{name}-{role}")
                })
                .unwrap();
            let key = if role == "http" { "api" } else { role };
            assert_eq!(
                route[key],
                format!(
                    "http://{}.{}.svc.cluster.local",
                    service["metadata"]["name"].as_str().unwrap(),
                    NAMESPACE
                )
            );
        }
        let row: Value = serde_json::from_str(row["data"]["release.json"].as_str().unwrap())?;
        let expected = selection
            .client
            .as_ref()
            .map(|client| (selection.hostname.clone(), client.web.clone()))
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(row["webs"], serde_json::to_value(expected)?);
        let expected = selection
            .client
            .as_ref()
            .map(|client| (selection.hostname.clone(), client.package.clone()))
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(row["clients"], serde_json::to_value(expected)?);
        Ok(carrier)
    };
    let before = releases
        .iter()
        .map(|(name, release)| Ok((*name, render(&document, name, release)?)))
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let release = releases.get_mut("orders").unwrap();
    release.composed_entry.as_mut().unwrap().reference = format!(
        "entries@{}",
        wamn_execution_contract::bytes_sha256(b"updated entry")
    );
    let selection = document.applications.get_mut("orders").unwrap();
    selection.previous = Some(selection.current.to_string());
    selection.current = DeclaredRelease::Digest(release.digest().as_str().to_owned());
    for (application, release) in &releases {
        let after = render(&document, application, release)?;
        if *application == "orders" {
            assert_ne!(before[application], after);
            let before_route = before[application]
                .iter()
                .find(|object| object["data"]["route.json"].is_string())
                .unwrap();
            let after_route = after
                .iter()
                .find(|object| object["data"]["route.json"].is_string())
                .unwrap();
            assert_eq!(before_route["data"], after_route["data"]);
        } else {
            assert_eq!(before[application], after);
        }
    }
    Ok(())
}

fn fixture_secrets(document: &EnvironmentDocument) -> Vec<Value> {
    let mut secrets = credential_secrets(&document.triple(), NAMESPACE);
    secrets.extend(operator_prerequisites(NAMESPACE));
    // The release host requires registry credentials before installation.
    secrets.push(prerequisite(
        "apply-registry",
        &["config.json", "ca.crt"],
        NAMESPACE,
    ));
    let mut oauth = prerequisite("wamn-oauth", &["key"], NAMESPACE);
    oauth["stringData"]["key"] = "o".repeat(32).into();
    secrets.push(oauth);
    // cert-manager issues the environment's NATS certificate, which the host
    // mounts as its data certificate (docs/plan/alignment.md R5).
    let env_key =
        wamn_control_registry::project_env_key(&document.org, &document.project, &document.env);
    secrets.push(prerequisite(
        &format!("wamn-nats-{env_key}"),
        &["ca.crt", "tls.crt", "tls.key"],
        NAMESPACE,
    ));
    secrets
}

#[test]
fn prekind_environment_renders() -> anyhow::Result<()> {
    let started = Instant::now();
    let work = ScratchRoot::create()?;
    let [a, b, h, http] = fixture_releases()?;
    for (name, env, bytes, drain) in [
        ("dev A", "dev", &a, 70),
        ("dev B", "dev", &b, 70),
        ("rollback A", "dev", &a, 70),
        ("dev short-drain", "dev", &a, 30),
        ("qa", "qa", &a, 70),
        ("HTTP fixture", "http", &http, 70),
        ("east H", "east", &h, 70),
        ("west H", "west", &h, 70),
    ] {
        let manifest = manifest(bytes)?;
        let document = fixture_document(
            env,
            &format!("{env}.orders.example"),
            manifest.digest().as_str(),
        );
        let group = fixture_host_group(&document, "registry.invalid");
        let path = work.path().join(format!("{env}-event-host-group.yaml"));
        std::fs::write(&path, serde_yaml::to_string(&group)?)?;
        let selected = if name == "rollback A" {
            rollback_host_group(work.path())
        } else {
            path
        };
        let values = render_values(&document, &manifest, &selected, drain)?;
        let documents = template(&values, NAMESPACE)?;
        assert_complete(&documents, &fixture_secrets(&document))
            .with_context(|| name.to_owned())?;
        println!(
            "{name}: host-group.yaml + {}",
            selected.file_name().unwrap().to_string_lossy()
        );
    }
    // Both connectors share integration.k and its session/event host overlay.
    for connector in ["rest_connector", "netsuite"] {
        let document = integration::fixture_document(connector);
        let group = fixture_host_group(&document, "registry.invalid");
        let path = work.path().join("integration-host-group.yaml");
        std::fs::write(&path, serde_yaml::to_string(&group)?)?;
        let mut release: Value = serde_json::from_slice(&a)?;
        release["components"] = json!([]);
        release["release"]["packages"] = serde_json::to_value(
            document
                .applications
                .values()
                .flat_map(|application| &application.packages)
                .map(|package| json!({"package-id":package, "package-version":"2.1.0"}))
                .collect::<Vec<_>>(),
        )?;
        let release = manifest(&wamn_execution_contract::canonical_json_bytes(&release))?;
        let values = render_values(&document, &release, &path, 70)?;
        assert_complete(&template(&values, NAMESPACE)?, &fixture_secrets(&document))
            .with_context(|| connector.to_owned())?;
        println!(
            "{connector}: integration.k + integration-host-group.yaml with session and events"
        );
    }
    println!(
        "prekind environment renders elapsed {:?}",
        started.elapsed()
    );
    Ok(())
}

#[test]
fn prekind_rejects_old_missing_conn_reader() -> anyhow::Result<()> {
    let work = ScratchRoot::create()?;
    let manifest = manifest(&fixture_releases()?[0])?;
    let document = fixture_document("dev", "dev.orders.example", manifest.digest().as_str());
    let group = fixture_host_group(&document, "registry.invalid");
    let path = rollback_host_group(work.path());
    std::fs::write(&path, serde_yaml::to_string(&group)?)?;
    let documents = template(&render_values(&document, &manifest, &path, 70)?, NAMESPACE)?;
    let mut secrets = fixture_secrets(&document);
    assert_complete(&documents, &secrets)?;
    let missing = wamn_control_provision::workload_secret_name(
        WorkloadRoleFamily::ConnectionReader,
        ORG,
        PROJECT,
        "dev",
    );
    secrets.retain(|secret| secret["metadata"]["name"] != missing);
    let error = assert_complete(&documents, &secrets).unwrap_err();
    assert!(
        error
            .to_string()
            .contains(&format!("missing Secret {NAMESPACE}/{missing}")),
        "{error:#}"
    );
    println!("0.1.121 proof: {error:#}");
    Ok(())
}

fn pat_release(project: &str) -> anyhow::Result<()> {
    use wamn_release_control::release_chart::ValuesInput;
    use wamn_test_infrastructure::rendering::{EventIdentity, HostRoleSecret, HostValuesInput};
    let scope = Triple::new("acme", project, "dev");
    let secrets = credential_secrets(&scope, NAMESPACE);
    let guest = wamn_control_provision::workload_secret_name(
        WorkloadRoleFamily::App,
        "acme",
        project,
        "dev",
    );
    let roles = wamn_control_provision::host_credentials::HOST_CREDENTIALS
        .iter()
        .map(|credential| credential.family)
        .filter(|family| {
            !matches!(
                family,
                WorkloadRoleFamily::App | WorkloadRoleFamily::RegistryReader
            )
        })
        .map(|family| HostRoleSecret {
            family,
            name: wamn_control_provision::workload_secret_name(family, "acme", project, "dev"),
        })
        .collect();
    let event = EventIdentity {
        org: "acme".into(),
        project: project.into(),
        environment: "dev".into(),
    };
    let manifest = manifest(&fixture_releases()?[2])?;
    let object_store_secret_name =
        (project == "wms").then(|| format!("wamn-object-store-credentials-acme--{project}--dev"));
    let mut secrets = secrets;
    if let Some(name) = &object_store_secret_name {
        secrets.push(prerequisite(name, &["credentials.json"], NAMESPACE));
    }
    let rendered = wamn_gate_harness::render_check::host_values(
        &HostValuesInput {
            namespace: NAMESPACE.into(),
            host_tag: "prekind".into(),
            replicas: 3,
            component_artifact_base: "registry.invalid/wamn/components".into(),
            release_artifact_base: "registry.invalid/wamn/releases".into(),
            manifest_digest: manifest.digest().as_str().into(),
            stream_replicas: 1,
            dup_window_secs: 120,
            event,
            guest_secret_name: guest,
            role_secrets: roles,
            object_store_secret_name,
        },
        project,
    )?;
    let roles = wamn_gate_harness::render_check::fixture_roles(&manifest, "acme", project, "dev");
    let values = wamn_release_control::release_chart::values(&ValuesInput {
        org: "acme".into(),
        project: project.into(),
        env: "dev".into(),
        manifest_digest: manifest.digest().as_str().into(),
        artifact_base: "registry.invalid/wamn/releases".into(),
        route_host: format!("{project}.example"),
        applications: [(
            project.into(),
            wamn_catalog::ApplicationDefinition {
                connections: std::collections::BTreeMap::new(),
                packages: manifest
                    .release
                    .packages
                    .iter()
                    .map(|package| package.package_id().to_owned())
                    .collect(),
            },
        )]
        .into(),
        roles,
        drain_bound_seconds: 70,
        actor: "prekind".into(),
        host_group: wamn_gate_harness::render_check::host_group(&rendered.overlay)?,
    })?;
    secrets.extend(operator_prerequisites(NAMESPACE));
    secrets.push(prerequisite("wamn-oauth", &["key"], NAMESPACE));
    // platform fixture startup emits the registry Secret, and cert-manager the
    // environment's NATS certificate, before either application installs its
    // release.
    secrets.extend([
        prerequisite("wamn-registry-pull", &[".dockerconfigjson"], NAMESPACE),
        prerequisite(
            &format!("wamn-nats-acme--{project}--dev"),
            &["ca.crt", "tls.crt", "tls.key"],
            NAMESPACE,
        ),
    ]);
    assert_complete(
        &template(&serde_yaml::to_value(values)?, NAMESPACE)?,
        &secrets,
    )?;
    println!("{project}: apply host values -> release_chart::values -> release chart");
    Ok(())
}

#[test]
fn prekind_receiving_pat_renders() -> anyhow::Result<()> {
    pat_release("receiving")
}

#[test]
fn prekind_wms_pat_renders() -> anyhow::Result<()> {
    pat_release("wms")
}

#[test]
fn host_report_release_requires_component_preload() -> anyhow::Result<()> {
    let device = release_documents::fixture_device()?;
    let bytes = release_documents::host_report_release("2.1.0", &device)?;
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)?;
    assert!(manifest.composed_entry.is_none());
    assert_eq!(wamn_execution_host::synchronous_route_count(&manifest), 1);
    let component = manifest.components.first().context("the served device")?;
    assert_eq!(
        component.descriptor.component_digest,
        device.component_digest
    );
    assert_eq!(component.descriptor.component, "device");
    assert!(manifest.routes.iter().all(|route| {
        route.component == component.descriptor.component
            && component.operations.contains_key(&route.operation)
    }));
    let roles = wamn_release_control::release_chart::roles(&manifest, ORG, PROJECT, "broken");
    assert!(
        roles
            .iter()
            .all(|role| role.name != wamn_release_control::release_chart::RoleName::Http),
        "an http role needs a composed entry that the preload would skip"
    );
    Ok(())
}

mod shared_resources {
    use serde::Deserialize as _;
    use serde_json::{Value, json};
    use wamn_environment_control::environment::{document::EnvironmentDocument, ensure};
    use wamn_release_control::release_chart::{self, ImageSet};
    use wamn_test_infrastructure::scratch::ScratchRoot;

    #[test]
    fn shared_materializer_has_stable_placement_outside_application_releases() -> anyhow::Result<()>
    {
        let mut document = EnvironmentDocument::from_json(br#"{"org":"acme","project":"apps","env":"dev","target":"cloud","policy":"dev","applications":{}}"#)?;
        let image = format!("registry.example/materializer@sha256:{}", "a".repeat(64));
        let workload = ensure::materializer_workload(&document, "hosts", &image)?;
        let values = ensure::materializer_host_values(&document, 30, 1, 120, None)?;
        document.applications = serde_json::from_value(json!({
            "a":{"packages":["a"],"current":format!("sha256:{}", "b".repeat(64)),"hostname":"a.example.com"},
            "b":{"packages":["b"],"current":format!("sha256:{}", "c".repeat(64)),"hostname":"b.example.com"}
        }))?;
        assert_eq!(
            workload,
            ensure::materializer_workload(&document, "hosts", &image)?
        );
        assert_eq!(
            values,
            ensure::materializer_host_values(&document, 30, 1, 120, None)?
        );
        let group = &values["runtime-operator"]["runtime"]["hostGroups"][0];
        assert_eq!(
            workload["spec"]["template"]["spec"]["hostSelector"],
            json!({"hostgroup":group["name"]})
        );
        assert_eq!(
            workload["spec"]["template"]["spec"]["service"]["image"],
            image
        );
        for application in document.applications.keys() {
            assert_ne!(
                group["name"],
                wamn_environment_control::environment::analyse::application_release_name(
                    &document,
                    application
                )?
            );
        }
        let scratch = ScratchRoot::create()?;
        let chart = scratch.path().join("chart");
        release_chart::stamp(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../deploy/platform/release"),
            &ImageSet {
                host: format!("registry.example/host:dev@sha256:{}", "d".repeat(64)),
                http: format!("registry.example/http@sha256:{}", "e".repeat(64)),
                materializer: image,
            },
            None,
            &chart,
        )?;
        let file = scratch.path().join("values.yaml");
        std::fs::write(&file, serde_yaml::to_string(&values)?)?;
        let output = std::process::Command::new("helm")
            .args(["template", group["name"].as_str().unwrap()])
            .arg(&chart)
            .args(["--namespace", "hosts", "--skip-crds", "-f"])
            .arg(file)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let objects = serde_yaml::Deserializer::from_str(std::str::from_utf8(&output.stdout)?)
            .map(Value::deserialize)
            .collect::<Result<Vec<_>, _>>()?;
        let host = objects
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .unwrap();
        let pod = &host["spec"]["template"];
        assert_eq!(
            pod["metadata"]["labels"]["wasmcloud.com/hostgroup"],
            group["name"]
        );
        assert!(pod["metadata"]["labels"].get("wamn.release").is_none());
        assert!(
            pod["metadata"]["annotations"]
                .get("wamn.release-digest")
                .is_none()
        );
        assert!(
            objects
                .iter()
                .all(|object| object["kind"] != "WorkloadDeployment")
        );
        assert!(
            !pod["spec"]["containers"][0]["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg.as_str().unwrap().starts_with("--release-"))
        );
        Ok(())
    }
}
