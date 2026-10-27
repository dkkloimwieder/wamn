//! Canonical release documents shared by kind and render-only cases.

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

pub const PACKAGE: &str = "edge_device";
/// The fixture release A: the fixture package, no http attachment, no
/// registration, a component no host pulls. `@VERSION@` is the package version.
pub const MANIFEST: &str = r#"{"attachments":{},"components":[{"descriptor":{"component":"http-request","component-digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{}},"operations":{"wamn:node/handler@0.1.0":{}},"package-id":"edge_device"}],"format-version":11,"release":{"packages":[{"package-id":"edge_device","package-version":"@VERSION@"}]},"routes":[]}"#;
/// The second fixture release, B: the same package, another component digest.
pub const MANIFEST_B: &str = r#"{"attachments":{},"components":[{"descriptor":{"component":"http-request","component-digest":"sha256:4444444444444444444444444444444444444444444444444444444444444444","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{}},"operations":{"wamn:node/handler@0.1.0":{}},"package-id":"edge_device"}],"format-version":11,"release":{"packages":[{"package-id":"edge_device","package-version":"@VERSION@"}]},"routes":[]}"#;
/// The third fixture release, H: the same package with one http attachment,
/// served with no authentication, so its environments render the http role.
/// Its one component, `null` here, is the admitted `device` component as
/// publish projects it; `@OPERATION@` is that component's one operation.
pub const MANIFEST_H: &str = r#"{"attachments":{"sample-read-http":{"auth-policy":{"modes":["none"]},"component":"device","definition":{"id":"sample-read-http","route":{"method":"GET","path":"/sample/read"},"run-deadline-ms":30000,"type":"http"},"definition-hash":"sha256:5555555555555555555555555555555555555555555555555555555555555555","operation":"@OPERATION@","package-id":"edge_device","type":"http"}},"components":[null],"format-version":11,"release":{"packages":[{"package-id":"edge_device","package-version":"@VERSION@"}]},"routes":[{"component":"device","operation":"@OPERATION@","package-id":"edge_device","type":"get"}]}"#;

/// The canonical bytes of a fixture release: `template` with the package
/// version, and with `device` as its one component, projected as publish
/// projects it, and that component's one operation as the operation of its
/// route. The host serves a route only from an operation that carries the
/// admitted operation's facts (`validate_component_in_release`).
pub fn release(
    template: &str,
    version: &str,
    device: Option<&wamn_catalog::ComponentDescriptor>,
) -> anyhow::Result<Vec<u8>> {
    let mut text = template.replace("@VERSION@", version);
    if let Some(device) = device {
        ensure!(
            device.operations.len() == 1,
            "the device descriptor has one operation"
        );
        let operation = device
            .operations
            .keys()
            .next()
            .context("the device operation")?;
        text = text.replace("@OPERATION@", operation);
    }
    let mut value: Value = serde_json::from_str(&text)?;
    value["release"]["packages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"package-id":"label_fixture","package-version":"1.0.0"}));
    if let Some(device) = device {
        let fact = device
            .clone()
            .into_admitted(wamn_catalog::ComponentPackageScope {
                env_key: String::new(),
                package_id: PACKAGE.to_owned(),
                package_version: version.to_owned(),
            })
            .component;
        value["components"][0] =
            serde_json::to_value(wamn_catalog::ServingComponent::project(&fact, Vec::new())?)?;
    }
    let bytes = wamn_execution_contract::canonical_json_bytes(&value);
    wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)
        .context("the fixture release is a valid canonical manifest")?;
    Ok(bytes)
}

pub fn fixture_device() -> anyhow::Result<wamn_catalog::ComponentDescriptor> {
    let (fixture, _) =
        wamn_catalog::ServingManifest::from_canonical_bytes(&release(MANIFEST, "2.1.0", None)?)?;
    let mut device =
        serde_json::to_value(&fixture.components.first().expect("a component").descriptor)
            .expect("a descriptor");
    device["component"] = json!("device");
    device["operations"] = json!({"edge-device:sample/read@2.1.0": {"registered-operation": wamn_catalog::operation_token("edge_device", "2.1.0", "sample", "read"),"input-ports": [], "output-ports": [], "parameters": []}});
    let device: wamn_catalog::ComponentDescriptor =
        serde_json::from_value(device).expect("a device descriptor");
    Ok(device)
}

pub fn fixture_releases() -> anyhow::Result<[Vec<u8>; 3]> {
    let device = fixture_device()?;
    let (h, _) = wamn_catalog::ServingManifest::from_canonical_bytes(
        &release(MANIFEST_H, "2.1.0", Some(&device)).expect("release H"),
    )
    .expect("release H");
    Ok([
        release(MANIFEST, "2.1.0", None)?,
        release(MANIFEST_B, "2.1.0", None)?,
        h.canonical_bytes(),
    ])
}
