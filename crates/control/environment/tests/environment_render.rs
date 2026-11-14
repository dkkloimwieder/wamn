//! Product environment rendering without a cluster or a running host.

#[path = "support/render_fixture.rs"]
pub mod render_fixture;
use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;
use wamn_environment::environment::document::EnvironmentDocument;
use wamn_registry::Triple;
#[path = "support/release_documents.rs"]
pub mod release_documents;
use release_documents::workflow as integration;
use release_documents::{
    NAMESPACE, ORG, PROJECT, environment_releases as fixture_releases, fixture_document,
    fixture_host_group,
};
use render_fixture::{
    assert_complete, credential_secrets, environment as render_values, operator_prerequisites,
    prerequisite, template,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

#[test]
fn prekind_deploy_connections_cover_the_wms_environment() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let document =
        EnvironmentDocument::compile(&repository.join("deploy/gcp/environments/dkk--dev--wms.k"))?;
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
        .require_definition(&connection["parameters"])
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
    use wamn_environment::environment::{document::DeclaredRelease, write};

    let mut releases = BTreeMap::new();
    let mut applications = serde_json::Map::new();
    for application in ["orders", "receiving"] {
        let mut release: wamn_catalog::ServingManifest = serde_json::from_value(json!({
            "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            "release": {"packages": [{"package-id": application, "package-version":"1.0.0"}]},
            "components": [], "routes": [],
            "attachments": {"http": {"type":"http", "package-id":application,
                "component":application, "operation":"get", "definition":{},
                "definition-hash":wamn_digest::bytes_sha256(application.as_bytes()),
                "auth-policy":{"modes":["none"]}}}
        }))?;
        let member = wamn_catalog::ServingEntryMember {
            package_id: application.into(),
            component: application.into(),
            digest: wamn_digest::bytes_sha256(application.as_bytes()),
        };
        release.composed_entry = Some(wamn_catalog::ServingComposedEntry {
            reference: format!("entries@{}", member.digest),
            route_digest: member.digest.clone(),
            application: member.clone(),
            members: [member].into(),
        });
        applications.insert(application.into(), json!({
            "revocation_seconds":5,"hostname":format!("{application}.example"),
            "client":{"package":application, "web":wamn_digest::bytes_sha256(application.as_bytes())}
        }));
        releases.insert(application, release);
    }
    let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org":ORG,"project":PROJECT,"env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,
        "applications":applications
    }))?)?;
    let render = |document: &EnvironmentDocument,
                  application: &str,
                  release: &wamn_catalog::ServingManifest|
     -> anyhow::Result<_> {
        let selection = &document.applications[application];
        let (values, _) = write::release_values(
            document,
            application,
            release,
            release.digest().as_str(),
            "registry.invalid/wamn/releases",
            "registry.invalid/wamn/components",
            "render",
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
        assert_eq!(row["digest"].as_str(), Some(release.digest().as_str()));
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
        Ok((
            row["digest"].clone(),
            route["host"].clone(),
            route["release"].clone(),
        ))
    };
    let before = releases
        .iter()
        .map(|(name, release)| Ok((*name, render(&document, name, release)?)))
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let release = releases.get_mut("orders").unwrap();
    release.composed_entry.as_mut().unwrap().reference =
        format!("entries@{}", wamn_digest::bytes_sha256(b"updated entry"));
    document.applications.get_mut("orders").unwrap().current =
        DeclaredRelease::Digest(release.digest().to_string());
    for (application, release) in &releases {
        let after = render(&document, application, release)?;
        let (before_digest, before_host, before_release) = &before[application];
        let (after_digest, after_host, after_release) = after;
        assert_eq!(after_host, *before_host);
        assert_eq!(after_release, *before_release);
        if *application == "orders" {
            assert_ne!(after_digest, *before_digest);
        } else {
            assert_eq!(after_digest, *before_digest);
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
    let project_env_key =
        wamn_registry::project_env_key(&document.org, &document.project, &document.env);
    secrets.push(prerequisite(
        &wamn_registry::name::nats_certificate(&project_env_key),
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
        let release = manifest(&wamn_digest::canonical_json_bytes(&release))?;
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
    let target = Triple::new("acme", "dev", project);
    let secrets = credential_secrets(&target, NAMESPACE);
    let manifest = manifest(&fixture_releases()?[2])?;
    let object_store_secret_name = (project == "wms").then(|| {
        wamn_registry::name::workload_secret_name(
            "wamn-object-store-credentials-",
            "acme",
            project,
            "dev",
        )
    });
    let mut secrets = secrets;
    if let Some(name) = &object_store_secret_name {
        secrets.push(prerequisite(name, &["credentials.json"], NAMESPACE));
    }
    let roles = render_fixture::fixture_roles(&manifest, "acme", project, "dev");
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
        actor: "prekind".into(),
        component_base: "registry.invalid/wamn/components".into(),
    })?;
    secrets.extend(operator_prerequisites(NAMESPACE));
    secrets.push(prerequisite("wamn-oauth", &["key"], NAMESPACE));
    // The platform credential owner emits the registry Secret before applications install.
    let scratch = ScratchRoot::create()?;
    let cluster = wamn_environment::platform::document::ClusterDocument::compile(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/kind/cluster.k"),
    )?;
    let auth_file = scratch.path().join("registry.json");
    std::fs::write(
        &auth_file,
        serde_json::to_vec(&json!({"auths":{
            &cluster.registry:{"username":"platform", "password":"credential"}
        }}))?,
    )?;
    secrets.push(wamn_environment::platform::control::registry_pull_secret(
        &cluster, &auth_file,
    )?);
    secrets.push(prerequisite(
        &wamn_registry::name::nats_certificate(&wamn_registry::project_env_key(
            "acme", project, "dev",
        )),
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

#[tokio::test]
async fn unavailable_registry_configuration_refuses_lifecycle_operations() -> anyhow::Result<()> {
    use wamn_environment::environment::{self, Platform};
    let scratch = ScratchRoot::create()?;
    let digest = wamn_digest::bytes_sha256(b"selected release");
    let document = fixture_document("dev", "orders.example", &digest);
    let triple = document.triple();
    let application = document.applications.keys().next().unwrap();
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
        chart_download: None,
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
        environment::deploy(&platform, &triple, application, &digest)
            .await
            .is_err()
    );
    assert!(
        environment::status::status(&mut platform, &triple, "invalid database URL")
            .await
            .is_err()
    );
    Ok(())
}
