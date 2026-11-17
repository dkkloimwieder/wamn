//! The runtime schema assigned to the NetSuite component during release assembly.
#[path = "../../rest_connector/tests/support.rs"]
mod support;

use std::collections::BTreeSet;
use wamn_catalog::PackageCoordinate;
use wamn_publish::publish_release::{PublishReleaseRequest, assemble_release};

struct Fixture {
    args: PublishReleaseRequest,
    roots: Vec<std::path::PathBuf>,
    admissions: Vec<wamn_publish::component_admission::ComponentAdmission>,
}

fn fixture() -> Fixture {
    // support::admit writes one temporary declaration per process and component.
    static ADMISSION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _admission = ADMISSION.lock().unwrap();
    let repository = support::repository();
    let integration = repository.join("apps/wamn_integration");
    let consumer = repository.join("apps/rest_consumer_fixture");
    let connector = repository.join("apps/netsuite");
    let roots = vec![integration.clone(), consumer.clone(), connector.clone()];
    let base = support::admit(&integration, "wamn_integration", BTreeSet::new());
    let destination = support::admit(&consumer, "rest_consumer_fixture", BTreeSet::new());
    let component = support::admit(
        &connector,
        "netsuite",
        BTreeSet::from([*base.component_digest(), *destination.component_digest()]),
    );
    let admissions = vec![base, destination, component];
    let args = PublishReleaseRequest {
        applications: serde_json::from_value(serde_json::json!({"netsuite": {"packages": ["wamn_integration", "netsuite", "rest_consumer_fixture"]}})).unwrap(),
        application: None,
        org: "rest".parse().unwrap(),
        project: "publication".parse().unwrap(),
        environment: "test".parse().unwrap(),
        packages: ["wamn_integration", "netsuite", "rest_consumer_fixture"]
            .into_iter()
            .map(|name| PackageCoordinate::new(name.parse().unwrap(), "0.1.1".parse().unwrap()).unwrap())
            .collect(),
    };
    Fixture {
        args,
        roots,
        admissions,
    }
}

#[test]
#[ignore = "requires: input:netsuite"]
fn publication_selects_the_integration_runtime_schema() {
    let palette = support::palette();
    let Fixture {
        args,
        roots,
        admissions,
    } = fixture();
    let assembled = assemble_release(&args, &roots, &admissions, &palette).unwrap();
    assert_eq!(
        assembled
            .published
            .manifest
            .components
            .iter()
            .find(|component| component.descriptor.component == "netsuite")
            .unwrap()
            .runtime_schema
            .as_deref(),
        Some("wamn_integration")
    );
}
