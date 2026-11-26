use std::path::Path;

use futures_util::TryStreamExt as _;
use object_store::{ObjectStore as _, ObjectStoreExt as _, memory::InMemory};
use wamn_digest::{bytes_sha256, canonical_json_bytes};
use wamn_release::web_artifact::upload::upload_platform_ui_to_store;
use wamn_release::web_artifact::{RegistryWebArtifacts, pack_web_artifact, platform_ui::pack};
use wamn_test_infrastructure::publish::{Server, artifact_fixture, fixture_registry};
use wamn_test_infrastructure::scratch::ScratchRoot;

#[tokio::test]
async fn missing_corrupt_and_app_artifacts_write_nothing() {
    let scratch = ScratchRoot::create().unwrap();
    artifact_fixture(scratch.path(), false);
    let packed = pack(&scratch.path().join("dist"), &"1.2.3".parse().unwrap()).unwrap();
    let app = pack_web_artifact(scratch.path()).unwrap();
    for (responses, digest) in [
        (vec![(404, Vec::new())], &packed.digest),
        (
            vec![(200, packed.manifest.clone())],
            &"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse::<wamn_digest::Digest>()
                .unwrap(),
        ),
        (
            vec![(200, packed.manifest.clone()), (200, b"corrupt".to_vec())],
            &packed.digest,
        ),
        (vec![(200, app.manifest)], &app.digest),
    ] {
        let server = Server::start(responses).await;
        let store = InMemory::new();
        assert!(
            upload_platform_ui_to_store(
                &RegistryWebArtifacts(&fixture_registry(&scratch, &server)),
                digest,
                "",
                &store
            )
            .await
            .is_err()
        );
        assert!(
            store
                .list(None)
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn partial_upload_keeps_equal_objects_and_conflicts_never_publish_the_index() {
    let scratch = ScratchRoot::create().unwrap();
    artifact_fixture(scratch.path(), false);
    let packed = pack(&scratch.path().join("dist"), &"1.2.3".parse().unwrap()).unwrap();
    for conflict in [true, false] {
        let server = Server::start(vec![
            (200, packed.manifest.clone()),
            (200, packed.tar.clone()),
        ])
        .await;
        let store = InMemory::new();
        let root = format!("platform-ui/sha256-{}", packed.digest.hex());
        let mut archive = tar::Archive::new(packed.tar.as_slice());
        let mut asset = archive
            .entries()
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| {
                let path = entry.path().unwrap();
                path.starts_with("dist") && path.as_ref() != Path::new("dist/index.html")
            })
            .unwrap();
        let path = asset
            .path()
            .unwrap()
            .strip_prefix("dist")
            .unwrap()
            .to_path_buf();
        let mut original = Vec::new();
        if conflict {
            original.extend_from_slice(b"conflict");
        } else {
            std::io::Read::read_to_end(&mut asset, &mut original).unwrap();
        }
        let original_digest = bytes_sha256(&original);
        let key = object_store::path::Path::from(format!("{root}/{}", path.display()));
        store.put(&key, original.into()).await.unwrap();
        let result = upload_platform_ui_to_store(
            &RegistryWebArtifacts(&fixture_registry(&scratch, &server)),
            &packed.digest,
            "",
            &store,
        )
        .await;
        assert_eq!(result.is_err(), conflict);
        assert_eq!(
            store
                .head(&format!("{root}/index.html").into())
                .await
                .is_err(),
            conflict
        );
        assert_eq!(
            bytes_sha256(&store.get(&key).await.unwrap().bytes().await.unwrap()),
            original_digest
        );
    }
}

#[tokio::test]
async fn upload_refuses_digest_mismatches_and_non_web_artifacts_before_any_write() {
    let scratch = ScratchRoot::create().unwrap();
    artifact_fixture(scratch.path(), false);
    let packed = pack_web_artifact(scratch.path()).unwrap();
    let mut non_web: serde_json::Value = serde_json::from_slice(&packed.manifest).unwrap();
    non_web["artifactType"] = serde_json::json!("application/vnd.wamn.package.v1");
    let non_web = canonical_json_bytes(&non_web);
    for (responses, digest, _reason) in [
        (
            vec![(200, packed.manifest.clone())],
            bytes_sha256(b"another manifest"),
            "Invalid digest",
        ),
        (
            vec![(200, packed.manifest.clone()), (200, b"wrong tar".to_vec())],
            packed.digest,
            "Invalid digest",
        ),
        (
            vec![(200, non_web.clone())],
            bytes_sha256(&non_web),
            "is not a",
        ),
    ] {
        let server = Server::start(responses).await;
        let store = InMemory::new();
        let _error = wamn_release::web_artifact::upload::upload_web_to_store(
            &RegistryWebArtifacts(&fixture_registry(&scratch, &server)),
            &digest,
            "",
            &store,
        )
        .await
        .unwrap_err();

        assert!(
            store
                .list(None)
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .is_empty()
        );
    }
}
