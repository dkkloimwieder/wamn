use std::collections::{BTreeMap, BTreeSet};
use wamn_catalog::PackageCoordinate;
use wamn_package_control::test_support::{closure_component, source_manifest};
use wamn_release_control::publish_release::{
    PublishReleaseManifest, ReleaseComponents, RouteContracts,
    publish_release_with_package_manifests,
};

#[test]
fn application_packages_with_different_model_schemas_publish_and_pass_apply_analysis() {
    use crate::environment::analyse::{
        Authorities, Installed, Observation, Policy, ProjectState, ReleaseFacts, notices, refusals,
        select_application,
    };
    let mut manifests = BTreeMap::new();
    let mut components = ReleaseComponents::default();
    let mut packages = BTreeSet::new();
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
        components
            .insert(component, Vec::new(), BTreeMap::new())
            .unwrap();
        packages.insert(PackageCoordinate::new(package, "1.0.0").unwrap());
        manifests.insert(package.to_owned(), manifest);
    }
    let published = publish_release_with_package_manifests(
        &PublishReleaseManifest {
            org: "tenant-a",
            project: "fixture",
            environment: "dev",
            packages: &packages,
            attachments: &BTreeMap::new(),
            environment_is_disposable: false,
        },
        &components,
        &manifests,
        &RouteContracts::new(),
    )
    .unwrap();
    assert_eq!(published.manifest.components.len(), 2);
    let mut document = crate::environment::document::EnvironmentDocument::from_json(
            serde_json::json!({"org":"acme", "project":"erp", "env":"dev",
                "target": "cloud", "policy":"dev",
                "applications": {"erp": {"current": published.digest.as_str(), "hostname": "erp.acme.example", "packages": ["consumer", "integration"]}}}).to_string().as_bytes()
        ).unwrap();
    let mut authorities = Authorities {
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
            required_connections: BTreeSet::new(),
        }),
        revision: None,
        live: BTreeMap::new(),
    };
    let mut unrelated = document.applications["erp"].clone();
    unrelated.packages = BTreeSet::from(["unrelated".into()]);
    document.applications.insert("other".into(), unrelated);
    assert!(select_application(&document, None).is_err());
    assert!(select_application(&document, Some("absent")).is_err());
    let selected = select_application(&document, Some("erp")).unwrap();
    assert!(refusals(&selected, &authorities).is_empty());
    assert_ne!(
        crate::environment::analyse::application_release_name(&document, "erp").unwrap(),
        crate::environment::analyse::application_release_name(&document, "other").unwrap()
    );
    let fingerprints: BTreeMap<_, _> = packages
        .iter()
        .map(|package| (package.package_id().to_owned(), "sha256:selected".into()))
        .collect();
    for package in &packages {
        authorities.project.as_mut().unwrap().installed.insert(
            package.package_id().to_owned(),
            Installed {
                current: "0.9.0".into(),
                physical_schema_fingerprint: "sha256:installed".into(),
            },
        );
    }
    let schema_notices = notices(&selected, "erp", &authorities, &fingerprints);
    for package in &packages {
        assert!(schema_notices.iter().any(|notice| {
            notice.application == "erp"
                && notice
                    .identity
                    .ends_with(&format!("/catalog.packages/{}", package.package_id()))
                && notice.observed
                    == Observation::SchemaDifference {
                        installed: Some("sha256:installed".into()),
                        selected: "sha256:selected".into(),
                    }
                && !notice.next_action.is_empty()
        }));
    }
    for installed in authorities.project.as_mut().unwrap().installed.values_mut() {
        installed.physical_schema_fingerprint = "sha256:selected".into();
    }
    assert!(notices(&selected, "erp", &authorities, &fingerprints).is_empty());
}
