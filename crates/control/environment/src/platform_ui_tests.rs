use crate::platform::web::{PLATFORM_UI_BUCKET, platform_ui_source};
use object_store::{ObjectStoreExt as _, memory::InMemory};
use wamn_package_control::web_artifact::write_platform_ui;

#[tokio::test]
async fn platform_ui_upload_uses_the_build_record_and_a_version_prefix_with_org_only() {
    use crate::platform_build::{
        BUILD_RECORD_FILE, PlatformBuildRecord, platform_ui_digest, platform_ui_dist,
    };
    let repository =
        std::env::temp_dir().join(format!("wamn-platform-upload-{}", std::process::id()));
    let dist = platform_ui_dist(&repository);
    let chart = repository.join("chart");
    std::fs::create_dir_all(dist.join("assets")).unwrap();
    std::fs::create_dir_all(&chart).unwrap();
    std::fs::write(dist.join("index.html"), b"platform index").unwrap();
    std::fs::write(dist.join("assets/ui.js"), b"platform script").unwrap();
    std::fs::write(chart.join("Chart.yaml"), "version: 0.1.7").unwrap();
    let platform = repository.join("deploy/platform/release");
    std::fs::create_dir_all(&platform).unwrap();
    std::fs::write(platform.join("Chart.yaml"), "version: 0.1.7").unwrap();
    std::fs::write(
        chart.join(BUILD_RECORD_FILE),
        serde_json::to_vec(&PlatformBuildRecord {
            platform_ui: Some(platform_ui_digest(&dist).unwrap()),
            stage_durations_ms: std::collections::BTreeMap::new(),
        })
        .unwrap(),
    )
    .unwrap();
    let (source, root) = platform_ui_source(&repository, &chart).unwrap();
    assert_eq!(source, dist);
    assert_eq!(root, "platform-ui/0.1.7");
    assert_eq!(PLATFORM_UI_BUCKET, "gs://wamn-dev-web");
    let store = InMemory::new();
    write_platform_ui(&store, &root, &source, "dkk")
        .await
        .unwrap();
    for (name, bytes) in [
        ("index.html", &b"platform index"[..]),
        ("assets/ui.js", &b"platform script"[..]),
        ("config.json", &br#"{"org":"dkk"}"#[..]),
    ] {
        let stored = store
            .get(&object_store::path::Path::from(format!("{root}/{name}")))
            .await
            .unwrap();
        assert_eq!(stored.bytes().await.unwrap().as_ref(), bytes);
    }
    std::fs::write(dist.join("assets/ui.js"), b"changed").unwrap();
    assert!(
        platform_ui_source(&repository, &chart)
            .unwrap_err()
            .to_string()
            .contains("differs from the platform build record")
    );
    std::fs::write(dist.join("assets/ui.js"), b"platform script").unwrap();
    std::fs::write(
        chart.join(BUILD_RECORD_FILE),
        serde_json::to_vec(&PlatformBuildRecord {
            platform_ui: None,
            stage_durations_ms: std::collections::BTreeMap::new(),
        })
        .unwrap(),
    )
    .unwrap();
    assert!(platform_ui_source(&repository, &chart).is_err());
    std::fs::remove_file(chart.join(BUILD_RECORD_FILE)).unwrap();
    assert!(
        platform_ui_source(&repository, &chart)
            .unwrap_err()
            .to_string()
            .contains("run `wamn-ctl build-platform`")
    );
    std::fs::remove_dir_all(repository).unwrap();
}
