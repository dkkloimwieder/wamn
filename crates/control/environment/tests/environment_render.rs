//! Product environment rendering without a cluster or a running host.

#[path = "support/render_check.rs"]
pub mod render_check;
use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;
use wamn_control_provision::WorkloadRoleFamily;
use wamn_control_registry::Triple;
use wamn_environment_control::enable_cdc_project_env::reader_deployment;
use wamn_environment_control::environment::document::EnvironmentDocument;
#[path = "support/release_documents.rs"]
pub mod release_documents;
use release_documents::workflow as integration;
use release_documents::{
    NAMESPACE, ORG, PROJECT, environment_releases as fixture_releases, fixture_document,
    fixture_host_group,
};
use render_check::{
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
            None,
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
        for workload in carrier
            .iter()
            .filter(|object| object["kind"] == "WorkloadDeployment")
        {
            for component in workload["spec"]["template"]["spec"]["components"]
                .as_array()
                .unwrap()
            {
                let resources = &component["localResources"];
                assert_eq!(resources["allowedIpNameLookups"], json!([]));
                for (key, value) in resources["environment"]["config"]
                    .as_object()
                    .into_iter()
                    .flatten()
                {
                    assert!(key != "DATABASE_URL" && !key.ends_with("_PG_URL"));
                    assert!(
                        !value
                            .as_str()
                            .and_then(|value| url::Url::parse(value).ok())
                            .is_some_and(|url| matches!(url.scheme(), "postgres" | "postgresql"))
                    );
                }
            }
        }
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
    document.applications.get_mut("orders").unwrap().current =
        DeclaredRelease::Digest(release.digest().to_string());
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

#[test]
fn reset_stops_workloads_and_reapply_restores_them() -> anyhow::Result<()> {
    use wamn_environment_control::environment::{ensure, sql::reset, write};
    let mut release = manifest(&fixture_releases()?[2])?;
    let component = release.components.iter().next().unwrap();
    let member = wamn_catalog::ServingEntryMember {
        package_id: component.package_id.clone(),
        component: component.descriptor.component.clone(),
        digest: component.descriptor.component_digest.clone(),
    };
    release.composed_entry = Some(wamn_catalog::ServingComposedEntry {
        reference: format!("entries@{}", member.digest),
        route_digest: member.digest.clone(),
        application: member.clone(),
        members: [member].into(),
    });
    let document = fixture_document("dev", "orders.example", release.digest().as_str());
    let application = document.applications.keys().next().unwrap();
    let scratch = ScratchRoot::create()?;
    let group = scratch.path().join("host.yaml");
    std::fs::write(
        &group,
        serde_yaml::to_string(&fixture_host_group(&document, "registry.invalid"))?,
    )?;
    let reapply = || -> anyhow::Result<_> {
        let (values, _) = write::release_values(
            &document,
            application,
            &release,
            release.digest().as_str(),
            "registry.invalid/releases",
            70,
            "reset-render",
            Some(&group),
            None,
        )?;
        Ok(values)
    };
    let mut stopped = serde_json::to_value(reapply()?)?;
    reset::stop_values(&mut stopped)?;
    let stopped = template(&serde_yaml::to_value(&stopped)?, NAMESPACE)?;
    assert!(
        stopped
            .iter()
            .all(|object| object["kind"] != "WorkloadDeployment")
    );
    let hosts = stopped
        .iter()
        .filter(|object| object["kind"] == "Deployment")
        .collect::<Vec<_>>();
    assert!(!hosts.is_empty());
    assert!(hosts.iter().all(|host| host["spec"]["replicas"] == 0));
    let resumed = template(&reapply()?, NAMESPACE)?;
    let workloads = resumed
        .iter()
        .filter(|object| object["kind"] == "WorkloadDeployment")
        .collect::<Vec<_>>();
    assert!(!workloads.is_empty());
    assert!(
        workloads
            .iter()
            .all(|workload| workload["spec"]["replicas"] == 1)
    );
    assert!(
        resumed
            .iter()
            .filter(|object| object["kind"] == "Deployment")
            .all(|host| host["spec"]["replicas"]
                .as_u64()
                .is_some_and(|replicas| replicas > 0))
    );

    // The separate materializer is deleted before its host is stopped, then
    // ordinary provisioning renders it again with its running replica count.
    let mut shared = ensure::materializer_host_values(
        &document,
        70,
        1,
        120,
        Some(serde_yaml::from_str(&std::fs::read_to_string(&group)?)?),
        false,
        None,
    )?;
    reset::stop_values(&mut shared)?;
    let shared = template(&serde_yaml::to_value(&shared)?, NAMESPACE)?;
    assert!(
        shared
            .iter()
            .all(|object| object["kind"] != "WorkloadDeployment")
    );
    assert!(
        shared
            .iter()
            .filter(|object| object["kind"] == "Deployment")
            .all(|host| host["spec"]["replicas"] == 0)
    );
    let shared = template(
        &serde_yaml::to_value(ensure::materializer_host_values(
            &document,
            70,
            1,
            120,
            Some(serde_yaml::from_str(&std::fs::read_to_string(&group)?)?),
            false,
            None,
        )?)?,
        NAMESPACE,
    )?;
    assert!(
        shared
            .iter()
            .filter(|object| object["kind"] == "Deployment")
            .all(|host| host["spec"]["replicas"]
                .as_u64()
                .is_some_and(|replicas| replicas > 0))
    );
    let image = format!("registry.invalid/materializer@sha256:{}", "a".repeat(64));
    let materializer = ensure::materializer_workload(&document, NAMESPACE, &image, false)?;
    assert_eq!(materializer["spec"]["replicas"], 1);
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
        let selected = path;
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
    let rendered = render_check::host_values(
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
    let roles = render_check::fixture_roles(&manifest, "acme", project, "dev");
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
        host_group: render_check::host_group(&rendered.overlay)?,
    })?;
    secrets.extend(operator_prerequisites(NAMESPACE));
    secrets.push(prerequisite("wamn-oauth", &["key"], NAMESPACE));
    // The platform credential owner emits the registry Secret before applications install.
    let scratch = ScratchRoot::create()?;
    let cluster = wamn_environment_control::platform::document::ClusterDocument::compile(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/kind/cluster.k"),
    )?;
    let auth_file = scratch.path().join("registry.json");
    std::fs::write(
        &auth_file,
        serde_json::to_vec(&json!({"auths":{
            &cluster.registry:{"username":"platform", "password":"credential"}
        }}))?,
    )?;
    secrets.push(
        wamn_environment_control::platform::control::registry_pull_secret(&cluster, &auth_file)?,
    );
    secrets.push(prerequisite(
        &format!("wamn-nats-acme--{project}--dev"),
        &["ca.crt", "tls.crt", "tls.key"],
        NAMESPACE,
    ));
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
        let workload = ensure::materializer_workload(&document, "hosts", &image, false)?;
        let resources = &workload["spec"]["template"]["spec"]["service"]["localResources"];
        assert_eq!(resources["allowedIpNameLookups"], json!([]));
        for (key, value) in resources["environment"]["config"].as_object().unwrap() {
            assert!(key != "DATABASE_URL" && !key.ends_with("_PG_URL"));
            assert!(
                !value
                    .as_str()
                    .and_then(|value| url::Url::parse(value).ok())
                    .is_some_and(|url| matches!(url.scheme(), "postgres" | "postgresql"))
            );
        }
        let values = ensure::materializer_host_values(&document, 30, 1, 120, None, false, None)?;
        document.applications = serde_json::from_value(json!({
            "a":{"packages":["a"],"current":format!("sha256:{}", "b".repeat(64)),"hostname":"a.example.com"},
            "b":{"packages":["b"],"current":format!("sha256:{}", "c".repeat(64)),"hostname":"b.example.com"}
        }))?;
        assert_eq!(
            workload,
            ensure::materializer_workload(&document, "hosts", &image, false)?
        );
        assert_eq!(
            values,
            ensure::materializer_host_values(&document, 30, 1, 120, None, false, None)?
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

#[tokio::test]
async fn unavailable_environment_authority_never_substitutes_checkout_selections()
-> anyhow::Result<()> {
    use wamn_environment_control::environment::{self, Platform};
    let scratch = ScratchRoot::create()?;
    let file = scratch.path().join("environment.k");
    let digest = wamn_execution_contract::bytes_sha256(b"selected release");
    let document = fixture_document("dev", "orders.example", &digest);
    std::fs::write(&file, document.to_kcl())?;
    let original = std::fs::read(&file)?;
    let kubeconfig = scratch.path().join("invalid-kubeconfig");
    std::fs::write(&kubeconfig, "invalid configuration")?;
    let mut platform = Platform {
        system_database_url: "invalid database URL".into(),
        target: wamn_release_control::release_chart::Target {
            kubeconfig,
            context: "absent".into(),
            namespace: NAMESPACE.into(),
        },
        chart: scratch.path().join("absent-chart"),
        release_artifact_base: "registry.invalid/releases".into(),
        registry_token_metadata: false,
        registry_auth_file: scratch.path().join("absent-auth.json"),
        oci_ca_paths: Vec::new(),
        insecure_registry: false,
        package_artifact_base: None,
        component_artifact_base: None,
        database_namespace: "platform".into(),
        database_host: None,
        database_port: 5432,
        host_group: None,
        pat_issuer: wamn_identity_client::PatIssuerConfig::default(),
        events: None,
        auth_nats: None,
    };
    assert!(
        environment::deploy(&platform, &file, &digest)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&file)?, original);
    assert!(
        environment::status::status(&mut platform, &file, "invalid database URL")
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&file)?, original);
    Ok(())
}

#[test]
fn reader_uses_the_environments_owned_credentials() {
    let scope = Triple::new("acme", "apps", "dev");
    let image = format!("registry/host@sha256:{}", "a".repeat(64));
    let first = reader_deployment(&scope, "hosts", &image, 1, 120);
    let pod = &first["spec"]["template"]["spec"];
    assert_eq!(pod["containers"][0]["image"], image);
    let variables = pod["containers"][0]["env"].as_array().unwrap();
    for (variable, secret) in [
        (
            "WAMN_CDC_URL",
            wamn_control_provision::project_env_cdc_secret_name(
                &scope.org,
                &scope.project,
                &scope.env,
            ),
        ),
        (
            "WAMN_SYSTEM_URL",
            wamn_control_provision::workload_secret_name(
                wamn_control_provision::WorkloadRoleFamily::RegistryReader,
                &scope.org,
                &scope.project,
                &scope.env,
            ),
        ),
    ] {
        let binding = variables
            .iter()
            .find(|entry| entry["name"] == variable)
            .unwrap();
        assert_eq!(binding["valueFrom"]["secretKeyRef"]["name"], secret);
        assert!(binding.get("value").is_none());
    }
    let other = reader_deployment(
        &Triple::new("acme", "apps", "other"),
        "hosts",
        &image,
        1,
        120,
    );
    assert_ne!(first["metadata"]["name"], other["metadata"]["name"]);
    assert_ne!(first["spec"]["selector"], other["spec"]["selector"]);
}
