//! The key reader accepts the permissions rendered for the control service.

use serde_json::Value;
use std::fs;
use std::path::Path;

#[test]
#[ignore = "requires: bin:helm"]
fn the_host_key_reader_accepts_the_rendered_secret_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    let chart = wamn_fixture_package::repository_root().join("deploy/charts/org");
    let metadata: Value =
        serde_yaml::from_slice(&fs::read(chart.join("Chart.yaml")).unwrap()).unwrap();
    let objects = wamn_kubernetes::helm::template(&serde_json::json!({
        "chart": chart, "version": metadata["version"], "name": "wamn-control", "namespace": "wamn-helm-acme",
        "values": {"org": "acme", "control": {
            "image": "wamn-control:dev",
            "configuration": {
                "identity_issuer": "https://identity.platform.svc.cluster.local",
                "nats_url": wamn_platform_install::platform::identity::NATS_URL,
                "database": {
                    "credentials": {"name": "wamn-control-db"},
                    "server_ca": {"name": "wamn-pg-ca"}
                }
            },
            "systemMigrationSecret": "wamn-platform-sql-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "registrySecret": "registry-pull",
            "oauthSecret": "wamn-oauth"
        }}
    })).expect("render the control service");
    let deployment = objects
        .iter()
        .find(|object| {
            object["kind"] == "Deployment"
                && object["metadata"]["name"] == "c-acme"
                && object["metadata"]["namespace"] == "acme"
        })
        .unwrap();
    let pod = &deployment["spec"]["template"]["spec"];
    let container = deployment["spec"]["template"]["spec"]["containers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|container| container["name"] == "control")
        .unwrap();
    let key = "/etc/wamn/oauth/key";
    let (mount, relative) = container["volumeMounts"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|mount| {
            Path::new(key)
                .strip_prefix(mount["mountPath"].as_str().unwrap())
                .ok()
                .map(|relative| (mount, relative))
        })
        .expect("the control key file is mounted");
    let secret = &pod["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|volume| volume["name"] == mount["name"])
        .expect("the key mount names a volume")["secret"];
    assert!(secret.is_object(), "the control key comes from a Secret");
    let item_mode = secret["items"].as_array().map(|items| {
        &items
            .iter()
            .find(|item| item["path"].as_str() == relative.to_str())
            .expect("the Secret projects the control key file")["mode"]
    });
    // Secret volumes default to 0644. For fsGroup the kubelet ORs 0440
    // into read-only volume files, including files with an explicit mode.
    let mut mode = item_mode
        .and_then(Value::as_u64)
        .or_else(|| secret["defaultMode"].as_u64())
        .unwrap_or(0o644);
    if pod["securityContext"]["fsGroup"].as_u64().is_some() {
        mode |= 0o440;
    }
    let work = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let path = work.path().join("key");
    fs::write(&path, [7; 32]).expect("write representative Secret key bytes");
    fs::set_permissions(
        &path,
        fs::Permissions::from_mode(u32::try_from(mode).unwrap()),
    )
    .expect("apply the kubelet's file mode");
    wamn_envelope::encryption::Envelope::from_file(&path)
        .unwrap_or_else(|error| panic!("rendered key {key} with mode {mode:o}: {error}"));
}
