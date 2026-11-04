use std::collections::{BTreeMap, BTreeSet};
use tokio_postgres::NoTls;
use wamn_catalog::PackageCoordinate;
use wamn_package_control::test_support::{DIGEST, closure_component, source_manifest};
use wamn_release_control::publish_release::{
    PublishReleaseManifest, ReleaseComponents, RouteContracts,
    publish_release_with_package_manifests,
};

#[tokio::test]
async fn two_applications_with_different_model_schemas_publish_and_pass_apply_analysis() {
    use crate::environment::analyse::{Authorities, Policy, ProjectState, ReleaseFacts};
    let _lock = wamn_test_postgres::lock();
    let database = wamn_catalog::test_database::tenant();
    let (client, connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "SELECT set_config('app.user_id', '{}', false)",
            wamn_control_provision::PlatformComponent::Provisioning.principal_id()
        ))
        .await
        .unwrap();
    let mut manifests = BTreeMap::new();
    let mut components = ReleaseComponents::default();
    let mut packages = BTreeSet::new();
    let mut digests = BTreeMap::new();
    for (package, schema, digest_byte) in [
        ("consumer", "consumer", 'a'),
        ("integration", "integration", 'b'),
    ] {
        let mut manifest = source_manifest();
        manifest.package.id = package.to_owned();
        for model in manifest.models.values_mut() {
            model.schema = schema.to_owned();
            model.owner = package.to_owned();
        }
        let mut component = closure_component("application", None);
        component.scope.package_id = package.to_owned();
        component.component_digest = format!("sha256:{}", digest_byte.to_string().repeat(64));
        let operation = format!("{package}:job/run@1.0.0");
        let mut export = component.operations.remove("run").unwrap();
        for (ports, name) in [
            (&mut export.input_ports, "input"),
            (&mut export.output_ports, "main"),
        ] {
            ports.push(wamn_catalog::AdmittedComponentPort {
                name: name.to_owned(),
                schema: wamn_catalog::ComponentSchema {
                    schema: serde_json::json!({}),
                },
            });
        }
        component.operations.insert(operation.clone(), export);
        client.execute("INSERT INTO catalog.packages (env_key,package_id,package_version,artifact_digest) VALUES ('tenant-a--fixture--dev',$1,'1.0.0',$2)", &[&package, &component.component_digest]).await.unwrap();
        digests.insert(package.to_owned(), component.component_digest.clone());
        components
            .insert(component, Vec::new(), BTreeMap::new())
            .unwrap();
        packages.insert(PackageCoordinate::new(package, "1.0.0").unwrap());
        manifests.insert(package.to_owned(), manifest);
    }
    let published = publish_release_with_package_manifests(
        &client,
        &PublishReleaseManifest {
            org: "tenant-a",
            project: "fixture",
            environment: "dev",
            verified_publisher_principal: "publisher",
            packages: &packages,
            attachments: &BTreeMap::new(),
            environment_is_disposable: false,
        },
        &components,
        &manifests,
        &digests,
        &RouteContracts::new(),
    )
    .await
    .unwrap();
    assert_eq!(published.manifest.components.len(), 2);
    let document = crate::environment::document::EnvironmentDocument::from_json(
            serde_json::json!({"org":"acme", "project":"erp", "env":"dev", "release":published.digest.as_str(),
                "target": "cloud", "route_host":"erp.acme.example", "policy":"dev",
                "applications": {"erp": {"packages": ["consumer", "integration"]}}}).to_string().as_bytes()
        ).unwrap();
    let authorities = Authorities {
        release_name: "acme--erp--dev".to_owned(),
        artifact_bindings: BTreeSet::new(),
        connections: BTreeMap::new(),
        policy: Policy {
            name: "dev".to_owned(),
            readiness_budget_seconds: 600,
            drain_bound_seconds: 300,
        },
        row: None,
        project: Some(ProjectState::default()),
        release: Some(ReleaseFacts {
            digest: published.digest.to_string(),
            manifest: published.manifest,
            image_set: wamn_release_control::release_chart::ImageSet {
                host: DIGEST.to_owned(),
                http: DIGEST.to_owned(),
                materializer: DIGEST.to_owned(),
            },
            qualification: "fixture".to_owned(),
            required_connections: BTreeSet::new(),
        }),
        revision: None,
        live: BTreeMap::new(),
    };
    assert!(crate::environment::analyse::refusals(&document, &authorities).is_empty());
}
