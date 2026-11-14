use object_store::memory::InMemory;
use wamn_package_artifact::registry_pull::test_server::Server;
use wamn_publish::web_artifact::test_support::{artifact_fixture, fixture_registry};
use wamn_publish::web_artifact::{pack_web_artifact, upload_web_to_store};
use wamn_test_infrastructure::scratch::ScratchRoot;

#[tokio::test]
async fn two_environments_with_the_same_web_digest_share_one_completed_upload() {
    use crate::environment::document::EnvironmentDocument;

    let scratch = ScratchRoot::create().unwrap();
    artifact_fixture(scratch.path(), false);
    let packed = pack_web_artifact(scratch.path()).unwrap();
    let server = Server::start(vec![
        (200, packed.manifest.clone()),
        (200, packed.tar.clone()),
        (200, packed.manifest.clone()),
        (200, packed.tar.clone()),
    ])
    .await;
    let registry = fixture_registry(&scratch, &server);
    let first: EnvironmentDocument = serde_json::from_value(serde_json::json!({
        "org": "acme", "project": "web-fixture", "env": "dev", "target": "dev", "policy": "dev","revocation_seconds":60,
        "applications": {"fixture": {"revocation_seconds":5,"hostname": "dev.example.com", "client": {"package": "web_fixture", "web": packed.digest}}}}))
    .unwrap();
    let mut second = first.clone();
    second.env = "staging".into();
    second.applications.get_mut("fixture").unwrap().hostname = "staging.example.com".into();
    let store = InMemory::new();
    let web = &first.applications["fixture"].client.as_ref().unwrap().web;
    let (package, root, written) = upload_web_to_store(&registry, web, "prefix", &store)
        .await
        .unwrap();
    assert!(written);
    assert_eq!(package, "web_fixture");
    assert_eq!(
        root,
        format!("prefix/clients/web_fixture/{}", &packed.digest[7..])
    );
    let web = &second.applications["fixture"].client.as_ref().unwrap().web;
    let (other_package, other_root, written) =
        upload_web_to_store(&registry, web, "prefix", &store)
            .await
            .unwrap();
    assert!(!written);
    assert_eq!(other_package, package);
    assert_eq!(other_root, root);
}
