//! The qualification artifact on a local `registry:2`
//! (docs/plan/platform-deploy.md §8.2, contract D, `wamn-t7gif.3`): the push
//! of `qualify-release`, its immutability, and the read of `env apply`.

use std::path::PathBuf;

use wamn_catalog::ServingManifest;
use wamn_control::delivery::qualification_artifact::{
    self, CaseResult, QualificationPayload, QualificationPushDisposition, Runner, image_set_digests,
};
use wamn_control::package_artifact::PackageRegistry;
use wamn_control::push_release_manifest::push_manifest_bytes;
use wamn_control::release_chart::ImageSet;

#[path = "support/local_registry.rs"]
mod local_registry;
use local_registry::LocalRegistry;

mod vector {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs"
    ));
}

fn image_set(http: char) -> ImageSet {
    ImageSet {
        host: format!("registry.example/host:1@sha256:{}", "b".repeat(64)),
        http: format!(
            "registry.example/http@sha256:{}",
            http.to_string().repeat(64)
        ),
        materializer: format!("registry.example/materializer@sha256:{}", "d".repeat(64)),
    }
}

fn payload(release: &str, set: &ImageSet, source_commit: &str) -> QualificationPayload {
    QualificationPayload {
        format: 1,
        release_digest: release.to_owned(),
        image_set: image_set_digests(set).expect("the image set is pinned"),
        chart_version: "0.1.0".to_owned(),
        cases: vec![CaseResult {
            name: "cluster::released_wms_routes".to_owned(),
            result: "pass".to_owned(),
        }],
        runner: Runner {
            source_commit: source_commit.to_owned(),
            image_digest: format!("sha256:{}", "9".repeat(64)),
        },
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_qualification_is_pushed_once_and_read_by_its_release_and_image_set() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("release-qualification-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create the scratch directory");
    let local = LocalRegistry::start("release-qualification");
    local.ready().await;
    let auth_file = root.join("config.json");
    std::fs::write(
        &auth_file,
        format!(
            r#"{{"auths":{{"{}":{{"username":"wamn","password":"wamn"}}}}}}"#,
            local.address
        ),
    )
    .expect("write the registry credential");
    let releases = PackageRegistry {
        artifact_base: format!("{}/wamn/releases", local.address),
        registry_auth_file: auth_file.clone(),
        insecure_registry: true,
        oci_ca_paths: Vec::new(),
        control_database_url: String::new(),
    };
    let (_, digest) = ServingManifest::from_canonical_bytes(vector::CANONICAL_BYTES)
        .expect("the vector is a canonical manifest");
    push_manifest_bytes(
        vector::CANONICAL_BYTES,
        &releases.artifact_base,
        true,
        &[],
        &auth_file,
    )
    .await
    .expect("publish-release pushes the release manifest");
    let release = digest.as_str();
    assert_eq!(release, vector::DIGEST);
    let size = vector::CANONICAL_BYTES.len();

    // apply refuses a release with no qualification and names the tag.
    let missing = qualification_artifact::find(&releases, release, &image_set('c'))
        .await
        .expect_err("no qualification yet");
    assert!(
        format!("{missing:#}").contains(&format!("qualification-{}-", &release[7..])),
        "{missing:#}"
    );

    // qualify-release pushes it; the same artifact again is already present.
    let first = payload(release, &image_set('c'), &"0".repeat(40));
    let (pushed, disposition) = qualification_artifact::push(&releases, &first, size)
        .await
        .expect("push the qualification");
    assert_eq!(disposition, QualificationPushDisposition::Pushed);
    let (again, disposition) = qualification_artifact::push(&releases, &first, size)
        .await
        .expect("the same qualification again");
    assert_eq!(
        (again.as_str(), disposition),
        (
            pushed.as_str(),
            QualificationPushDisposition::AlreadyPresent
        )
    );

    // The tag is immutable: another artifact under it refuses and names both.
    let other = payload(release, &image_set('c'), &"1".repeat(40));
    let refused = qualification_artifact::push(&releases, &other, size)
        .await
        .expect_err("another qualification under the same tag");
    let refused = format!("{refused:#}");
    assert!(
        refused.contains(&pushed) && refused.contains("immutable"),
        "{refused}"
    );

    // apply reads it by the release and the chart's image set, whatever
    // registry names the images.
    let mut renamed = image_set('c');
    renamed.http = format!("elsewhere.example/http@sha256:{}", "c".repeat(64));
    assert_eq!(
        qualification_artifact::find(&releases, release, &renamed)
            .await
            .expect("apply finds the qualification"),
        pushed
    );
    // Another role digest is another image set, with no qualification.
    assert!(
        qualification_artifact::find(&releases, release, &image_set('e'))
            .await
            .is_err()
    );

    drop(local);
    std::fs::remove_dir_all(&root).expect("remove the scratch directory");
}
