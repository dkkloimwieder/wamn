//! Product environment rendering without a cluster or a running host.

use serde_json::{Value, json};
use wamn_environment::environment::document::EnvironmentDocument;
use wamn_test_infrastructure::scratch::ScratchRoot;

const NAMESPACE: &str = "hosts";
const ORG: &str = "acme";
const PROJECT: &str = "orders";

#[test]
#[ignore = "requires: bin:helm"]
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
            package_id: application.parse().unwrap(),
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
                  application: &wamn_catalog::ApplicationName,
                  release: &wamn_catalog::ServingManifest|
     -> anyhow::Result<_> {
        let selection = &document.applications[application];
        let (values, _) = write::release_values(
            document,
            application,
            release,
            &release.digest(),
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
            [&serde_yaml::Value::from(application.as_str())]
        );
        assert_eq!(
            values["routeHost"].as_str(),
            Some(selection.hostname.as_str())
        );
        let work = ScratchRoot::create()?;
        let image = format!(
            "registry.invalid/wamn/fixture:render@sha256:{}",
            "1".repeat(64)
        );
        let chart = wamn_release_control::release_chart::stamp(
            &wamn_fixture_package::repository_root().join("deploy/charts/release"),
            &wamn_release_control::release_chart::ImageSet {
                host: image.clone(),
                http: image,
            },
            &work.path().join("chart"),
        )?;
        let metadata: Value = serde_yaml::from_slice(&std::fs::read(chart.join("Chart.yaml"))?)?;
        let carrier = wamn_kubernetes::helm::template(&json!({
            "chart": chart, "version": metadata["version"],
            "name": wamn_environment::environment::analyse::application_release_name(document, application)?,
            "namespace": NAMESPACE, "values": values,
        }))?;
        for workload in carrier
            .iter()
            .filter(|object| object["kind"] == "WorkloadDeployment")
        {
            for component in workload["spec"]["template"]["spec"]["components"]
                .as_array()
                .unwrap()
            {
                let resources = &component["localResources"];
                {
                    let observed = &(resources["allowedIpNameLookups"]);
                    assert!(observed.as_array().expect("array").is_empty());
                };
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
        assert_eq!(row["digest"], serde_json::json!(release.digest()));
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
        .map(|(name, release)| Ok((*name, render(&document, &name.parse().unwrap(), release)?)))
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let release = releases.get_mut("orders").unwrap();
    release.composed_entry.as_mut().unwrap().reference =
        format!("entries@{}", wamn_digest::bytes_sha256(b"updated entry"));
    document.applications.get_mut("orders").unwrap().current =
        DeclaredRelease::Digest(release.digest());
    for (application, release) in &releases {
        let after = render(&document, &application.parse().unwrap(), release)?;
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

#[tokio::test]
async fn unavailable_registry_configuration_refuses_lifecycle_operations() -> anyhow::Result<()> {
    use wamn_environment::environment::{self, Platform};
    let scratch = ScratchRoot::create()?;
    let digest = wamn_digest::bytes_sha256(b"selected release");
    let document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org": ORG, "project": PROJECT, "env": "dev", "target": "cloud",
        "policy": "dev", "revocation_seconds": 60,
        "applications": {"orders": {"hostname": "orders.example", "revocation_seconds": 5}}
    }))?)?;
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
