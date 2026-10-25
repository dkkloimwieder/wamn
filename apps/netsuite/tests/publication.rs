//! Publication refusals and the checkpoint command through the native host.
mod checkpoint_live;
#[path = "../../rest_connector/tests/consumer_live.rs"]
#[expect(
    dead_code,
    reason = "this target shares installation, not the REST consumer invocation"
)]
mod consumer_live;
#[path = "../../rest_connector/tests/support.rs"]
mod support;

use std::collections::BTreeSet;
use wamn_catalog::{ComponentPackageScope, PackageCoordinate, WiringDocument};
use wamn_control::publish_release::{
    PublishReleaseRequest, ReleaseWiringTarget, assemble_local_release,
};
use wamn_schema_generator::operation_reference::{AuthoredDocument, read_authored_document};

struct Fixture {
    args: PublishReleaseRequest,
    roots: Vec<std::path::PathBuf>,
    admissions: Vec<wamn_control::component_admission::ComponentAdmission>,
    documents: Vec<(ComponentPackageScope, WiringDocument)>,
}

fn fixture() -> Fixture {
    // support::admit writes one temporary declaration per process and component.
    static ADMISSION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _admission = ADMISSION.lock().unwrap();
    let repository = support::repository();
    let integration = repository.join("apps/platform/wamn_integration");
    let consumer = repository.join("apps/rest_consumer_fixture");
    let connector = repository.join("apps/netsuite");
    let roots = vec![integration.clone(), consumer.clone(), connector.clone()];
    let base = support::admit(&integration, "wamn_integration", None, BTreeSet::new());
    let destination = support::admit(&consumer, "rest_consumer_fixture", None, BTreeSet::new());
    let component = support::admit(
        &connector,
        "netsuite",
        None,
        BTreeSet::from([
            base.component_digest().into(),
            destination.component_digest().into(),
        ]),
    );
    let admissions = vec![
        support::admit(
            &connector,
            "http-request",
            Some("apps/platform/no-std/http-request/declaration.json.in"),
            BTreeSet::new(),
        ),
        base,
        destination,
        component,
        support::admit(
            &connector,
            "http-page",
            Some("apps/platform/no-std/http-page/declaration.json.in"),
            BTreeSet::new(),
        ),
        support::admit(
            &connector,
            "jsonata",
            Some("apps/platform/execution/jsonata/declaration.json.in"),
            BTreeSet::new(),
        ),
    ];
    let scope = ComponentPackageScope {
        tenant_id: "rest--publication--test".into(),
        package_id: "netsuite".into(),
        package_version: "0.1.0".into(),
    };
    let documents: Vec<_> = [
        "netsuite_pull",
        "netsuite_push",
        "record_get",
        "record_create",
        "record_update",
        "suiteql_query",
        "restlet_call",
    ]
    .into_iter()
    .map(|name| {
        let document: WiringDocument = serde_json::from_value(
            read_authored_document(
                &connector.join(format!("publication/wirings/{name}.json")),
                AuthoredDocument::Wiring,
            )
            .unwrap(),
        )
        .unwrap();
        (scope.clone(), document)
    })
    .collect();
    let args = PublishReleaseRequest {
        database_url: String::new(),
        control_database_url: String::new(),
        org: "rest".into(),
        tenant: scope.tenant_id.clone(),
        environment: "test".into(),
        verified_publisher_principal: "publisher".into(),
        run_schema: "catalog".into(),
        packages: ["wamn_integration", "netsuite", "rest_consumer_fixture"]
            .into_iter()
            .map(|name| PackageCoordinate::new(name, "0.1.0").unwrap())
            .collect(),
        wirings: documents
            .iter()
            .map(|(_, document)| ReleaseWiringTarget {
                package_id: scope.package_id.clone(),
                package_version: scope.package_version.clone(),
                wiring_id: document.wiring_id.clone(),
                wiring_version: document.version,
            })
            .collect(),
    };
    Fixture {
        args,
        roots,
        admissions,
        documents,
    }
}

#[tokio::test]
async fn pull_checkpoint_commits_through_the_application_role() -> anyhow::Result<()> {
    let Fixture {
        args,
        roots,
        admissions,
        documents,
    } = fixture();
    let assembled = assemble_local_release(&args, &roots, &admissions, documents)?;
    checkpoint_live::invoke(&roots, &assembled.published.manifest, &admissions).await
}

#[test]
fn invalid_ports_and_dependencies_refuse_publication() {
    let Fixture {
        args,
        roots,
        admissions,
        documents,
    } = fixture();
    let assembled = assemble_local_release(&args, &roots, &admissions, documents.clone()).unwrap();
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
    let mut wrong_port = documents.clone();
    wrong_port[0]
        .1
        .edges
        .iter_mut()
        .find(|edge| edge.from == "rows")
        .unwrap()
        .from_port = "main".into();
    assert!(assemble_local_release(&args, &roots, &admissions, wrong_port).is_err());
    let mut wrong_dependency = documents;
    wrong_dependency[0]
        .1
        .nodes
        .get_mut("record")
        .unwrap()
        .operation_dependency
        .as_mut()
        .unwrap()
        .operation = "item.other".into();
    assert!(assemble_local_release(&args, &roots, &admissions, wrong_dependency).is_err());
}
