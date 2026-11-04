use object_store::{ObjectStore as _, ObjectStoreExt as _, memory::InMemory};
use wamn_package_artifact::registry_pull::test_server::Server;
use wamn_package_control::web_artifact::test_support::{artifact_fixture, fixture_registry};
use wamn_package_control::web_artifact::{pack_web_artifact, upload_web_to_store};
use wamn_test_infrastructure::scratch::ScratchRoot;
const HEAD: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[tokio::test]
async fn two_environments_with_the_same_web_digest_share_one_completed_upload() {
    use crate::environment::document::EnvironmentDocument;
    use futures_util::TryStreamExt as _;

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
        "org": "acme", "project": "web-fixture", "env": "dev", "target": "dev",
        "release": HEAD, "route_host": "dev.example.com", "policy": "dev",
        "applications": {}, "client": {"package": "web_fixture", "web": packed.digest},
    }))
    .unwrap();
    let mut second = first.clone();
    second.env = "staging".into();
    second.route_host = "staging.example.com".into();
    let store = InMemory::new();
    let upload = |environment: &EnvironmentDocument| {
        (
            environment.client.as_ref().unwrap().web.clone(),
            environment.org.clone(),
        )
    };
    let (web, org) = upload(&first);
    let (package, root, written) = upload_web_to_store(&registry, &web, &org, "prefix", &store)
        .await
        .unwrap();
    assert!(written);
    assert_eq!(package, "web_fixture");
    assert_eq!(
        root,
        format!("prefix/clients/web_fixture/{}", &packed.digest[7..])
    );
    let before = store.list(None).try_collect::<Vec<_>>().await.unwrap();
    assert_eq!(before.len(), 3);
    let scope = store
        .get(&object_store::path::Path::from(format!(
            "{root}/config.json"
        )))
        .await
        .unwrap();
    assert_eq!(
        scope.bytes().await.unwrap().as_ref(),
        br#"{"org":"acme","project":"web-fixture"}"#
    );
    let (web, org) = upload(&second);
    let (other_package, other_root, written) =
        upload_web_to_store(&registry, &web, &org, "prefix", &store)
            .await
            .unwrap();
    assert!(!written);
    assert_eq!(other_package, package);
    assert_eq!(other_root, root);
    assert_eq!(
        store.list(None).try_collect::<Vec<_>>().await.unwrap(),
        before
    );
}
