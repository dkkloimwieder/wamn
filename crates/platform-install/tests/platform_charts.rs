//! Platform chart interfaces that require KCL and Helm.

use std::path::Path;

use serde_json::json;

#[test]
#[ignore = "requires: bin:kcl, bin:helm"]
fn platform_edge_uses_the_selected_image_and_existing_certificate() -> anyhow::Result<()> {
    use wamn_platform_install::platform::{
        document::{ClusterDocument, Provider},
        edge,
    };

    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut document = ClusterDocument::compile(&repository.join("deploy/kind/cluster.k"))?;
    for provider in [Provider::Kind, Provider::Gcp] {
        document.provider = provider;
        document.buckets.web = if provider == Provider::Gcp {
            "fixture-web".into()
        } else {
            String::new()
        };
        let entry = edge::chart_input(
            &repository.join("deploy/charts/edge"),
            "registry.invalid/edge@sha256:fixture",
            &document,
        )?;
        let objects = wamn_kubernetes::helm::template(&entry)?;
        let certificate_entry = wamn_platform_install::environment::entry(
            &repository.join("deploy/charts/installation"),
            "platform",
            "wamn",
            json!({"org":document.org,"registry":document.registry,"edgeDnsNames":["*.example.com"]}),
        )?;
        let certificates = wamn_kubernetes::helm::template(&certificate_entry)?;
        let certificate = certificates
            .iter()
            .find(|object| {
                object["kind"] == "Certificate"
                    && object["spec"]["secretName"] == entry["values"]["tlsSecret"]
            })
            .unwrap();
        assert!(!objects.iter().any(|object| object["kind"] == "Certificate"));
        let deployments: Vec<_> = objects
            .iter()
            .filter(|object| object["kind"] == "Deployment")
            .collect();
        assert_eq!(deployments.len(), 1);
        let deployment = deployments[0];
        assert_eq!(
            certificate["metadata"]["namespace"],
            deployment["metadata"]["namespace"]
        );
        let pod = &deployment["spec"]["template"]["spec"];
        if provider == Provider::Kind {
            let ingress = objects
                .iter()
                .find(|object| object["kind"] == "Ingress")
                .unwrap();
            assert_eq!(
                ingress["spec"]["tls"][0]["secretName"],
                certificate["spec"]["secretName"]
            );
            assert_eq!(
                ingress["spec"]["defaultBackend"]["service"]["name"],
                deployment["metadata"]["name"]
            );
        }
        let role = objects
            .iter()
            .find(|object| object["kind"] == "ClusterRole")
            .unwrap();
        assert!(role["metadata"]["namespace"].is_null());
        let binding = objects
            .iter()
            .find(|object| object["kind"] == "ClusterRoleBinding")
            .unwrap();
        assert_eq!(binding["subjects"][0]["name"], pod["serviceAccountName"]);
        let account = objects
            .iter()
            .find(|object| object["kind"] == "ServiceAccount")
            .unwrap();
        assert_eq!(account["metadata"]["name"], pod["serviceAccountName"]);
        assert_eq!(
            account["metadata"]["namespace"],
            deployment["metadata"]["namespace"]
        );
        assert_eq!(binding["roleRef"]["name"], role["metadata"]["name"]);
        let config = objects
            .iter()
            .find(|object| object["kind"] == "ConfigMap")
            .unwrap();
        let web: serde_json::Value =
            serde_json::from_str(config["data"]["web.json"].as_str().unwrap())?;
        assert_eq!(web["identity"], entry["values"]["identity"]);
    }
    Ok(())
}

#[test]
#[ignore = "requires: bin:kcl, bin:helm"]
fn org_chart_uses_each_recovery_owners_policy_and_preserves_role_references() -> anyhow::Result<()>
{
    let template = wamn_control_provision::Template::standard();
    let (org, _) = template.stamp("acme".parse().unwrap(), "wamn-pg");
    let mut values = wamn_platform_install::org::values(&org, &template.policies)?;
    let cluster = values["clusters"][0]["name"].as_str().unwrap().to_owned();
    let role = wamn_control_provision::connection::managed_role(
        "application".parse()?,
        "wamn_app",
        "application-login".into(),
    );
    values["managedRoles"] = json!({cluster: [role]});
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut entry = wamn_platform_install::environment::entry(
        &repository.join("deploy/charts/org"),
        org.id.as_str(),
        "org-acme",
        values,
    )?;
    let prepared =
        wamn_platform_install::platform::artifact_plan::test_util::prepared_chart(&repository)?;
    let chart = wamn_platform_install::platform::artifact_plan::select(&prepared.0)?;
    let mut document = wamn_platform_install::platform::document::ClusterDocument::compile(
        &repository.join("deploy/kind/cluster.k"),
    )?;
    document.org.clone_from(&org.id);
    let entry = wamn_platform_install::platform::installation::organization_entry(
        &document,
        &chart,
        Some(entry["values"].take()),
    )?;
    let objects = wamn_kubernetes::helm::template(&entry)?;
    assert!(
        objects
            .iter()
            .filter(|object| object["kind"] == "Cluster")
            .flat_map(|object| object["spec"]["managed"]["roles"].as_array().unwrap())
            .all(|role| role["name"] != "application")
    );
    for input in entry["values"]["clusters"].as_array().unwrap() {
        let cluster = objects
            .iter()
            .find(|object| {
                object["kind"] == "Cluster" && object["metadata"]["name"] == input["name"]
            })
            .unwrap();
        assert_eq!(cluster["metadata"]["namespace"], org.id.as_str());
        assert_eq!(cluster["spec"]["instances"], input["policy"]["instances"]);
        assert_eq!(
            cluster["spec"]["storage"]["size"],
            input["policy"]["storage"]
        );
        if let Some(roles) =
            entry["values"]["managedRoles"][input["name"].as_str().unwrap()].as_array()
        {
            for role in roles {
                assert!(
                    cluster["spec"]["managed"]["roles"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|declared| declared["name"] == role["name"]
                            && declared["passwordSecret"] == role["passwordSecret"])
                );
            }
        }
    }
    Ok(())
}
