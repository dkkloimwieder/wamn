use std::collections::BTreeMap;

use serde_json::json;
use wamn_component_policy::PackageManifest;
use wamn_digest::{Digest, bytes_sha256};
use wamn_platform_install::release_chart::{self, ImageSet, bundle};
use wamn_test_infrastructure::scratch::ScratchRoot;

use super::{Binding, EnvDocument, release_values, workload_images};

#[test]
fn workload_images_keep_each_repository_and_digest() -> anyhow::Result<()> {
    let org_origin = "org.example/artifacts";
    let platform_origin = "platform.example/shared";
    let (_, app_repository) = wamn_registry::org_artifact_bases(org_origin);
    let integration_repository = format!("{platform_origin}/wamn/integrations");
    let component_digest = bytes_sha256(b"component bytes");
    let component_image = bytes_sha256(b"component image");
    let integration_image = bytes_sha256(b"integration image");
    let image_set = ImageSet {
        host: format!("{platform_origin}/host:fixture@{}", bytes_sha256(b"host")),
        ingress: format!(
            "{platform_origin}/wamn/ingress@{}",
            bytes_sha256(b"ingress")
        ),
        workflow: format!(
            "{platform_origin}/wamn/workflow@{}",
            bytes_sha256(b"workflow")
        ),
    };
    let scratch = ScratchRoot::create()?;
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let chart = scratch.path().join("chart");
    release_chart::stamp(&repository.join("deploy/charts/app"), &image_set, &chart)?;
    let defaults = chart.join("values.yaml");
    let mut values: serde_json::Value = serde_yaml::from_slice(&std::fs::read(&defaults)?)?;
    for name in ["ctl", "control", "identity", "edge"] {
        values["platform"][name]["image"] = json!(image_set.host);
    }
    std::fs::write(defaults, serde_yaml::to_string(&values)?)?;
    bundle::prepare_payload(&repository, &chart)?;
    let chart = bundle::select(&chart)?;

    let document = EnvDocument::from_json(
        br#"{"org":"acme","project":"inventory","env":"dev","target":"cloud",
             "revocation_seconds":30,"apps":{"shop":{}}}"#,
    )?;
    let app = document.apps.first_key_value().unwrap().0;
    let manifest: PackageManifest = serde_json::from_value(json!({
        "package": {"id": "inventory", "version": "1.0.0"},
        "required_platform_policy_contract": {"id": "inventory", "state": "satisfied"},
        "models": {"item": {
            "schema": "inventory", "table": "item", "owner": "inventory", "operations": {}
        }},
        "connections": {},
        "components": {"guest": {}},
        "deploy": {"routes": {}, "components": {"guest": {
            "descriptor": {
                "component": "guest", "interface-version": "1.0.0",
                "component-digest": component_digest, "operations": {},
                "imports": [], "imports-fingerprint": component_digest,
                "effects": [], "connections": [],
            },
            "image_digest": component_image,
        }}},
    }))?;
    let component = manifest
        .deploy
        .as_ref()
        .unwrap()
        .components
        .values()
        .next()
        .unwrap();
    let binding = Binding {
        app: app.clone(),
        alias: "meter".into(),
        interface: "wamn:modbus/read@1.0.0".into(),
        components: [crate::app_chart::plan::component_name(
            &manifest.package.id,
            &component.descriptor.component,
        )]
        .into(),
        runtime_type: wamn_connection_type::RuntimeType::Wasm,
        target_type: "local".into(),
        connection_id: "meter-connection".into(),
        authority: Some("meter.example:502".into()),
        integration_type: "modbus".parse()?,
        exclusive: true,
        generation: 1,
        mode: wamn_package::manifest::ConnectionMode::Read,
        digest: Some(integration_image),
        exports: Vec::new(),
        write_interfaces: Vec::new(),
        resolved: None,
    };
    let manifests = BTreeMap::from([(manifest.package.id.clone(), manifest)]);
    let values = release_values(
        &document,
        "acme--dev",
        app,
        &manifests,
        &bytes_sha256(b"package"),
        &[binding],
        &integration_repository,
        &app_repository,
        |_| Ok(502),
    )?;
    assert_eq!(values["namespace"].as_str(), Some("acme--dev"));
    let mut images = workload_images(&values, &chart)?;
    assert!(images.remove(image_set.ingress.as_str()));
    assert!(images.remove(image_set.workflow.as_str()));

    let repositories = images
        .into_iter()
        .map(|image| {
            let (repository, digest) = image.rsplit_once('@').unwrap();
            Ok((digest.parse::<Digest>()?, repository))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    assert_eq!(
        repositories,
        BTreeMap::from([
            (component_image, app_repository.as_str()),
            (integration_image, integration_repository.as_str()),
        ])
    );
    Ok(())
}
