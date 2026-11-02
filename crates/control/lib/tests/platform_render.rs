//! Platform objects rendered without a cluster.
use serde_json::json;
use wamn_control as control;
#[path = "support/platform_install.rs"]
mod platform_install;
use std::path::Path;
use std::time::Instant;
#[path = "support/release_documents.rs"]
pub mod release_documents;
use release_documents::fixture_environment;

#[test]
fn prekind_platform_renders() -> anyhow::Result<()> {
    use wamn_gate_harness::render_check::{
        assert_complete, credential_secrets, environment, operator_prerequisites, template,
    };
    let started = Instant::now();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let work = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    // The platform publishes built application releases in kind. A fixture H
    // document renders the same HTTP and materializer role shapes without Wasm.
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(
        &release_documents::fixture_releases()?[2],
    )?;
    for project in ["receiving", "wms"] {
        let (_, mut document) = fixture_environment(&repository, project)?;
        document.org = "test".into();
        let path = repository.join(format!("deploy/gcp/host-group-{project}.yaml"));
        let group = wamn_control::platform::render_host_group(
            &std::fs::read_to_string(&path)?,
            &document,
            "registry.invalid/wamn",
            "render",
        )?;
        let path = work.path().join(format!("host-group-{project}.yaml"));
        std::fs::write(&path, serde_yaml::to_string(&group)?)?;
        let mut secrets = credential_secrets(&document.triple(), "hosts");
        secrets.extend(operator_prerequisites("hosts"));
        secrets.extend(wamn_control::platform::render_event_secrets(&document)?);
        // cert-manager issues the environment's NATS certificate Secret.
        secrets.push(json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": format!("wamn-nats-test--{project}--dev"), "namespace": "hosts"},
            "data": {"ca.crt": "", "tls.crt": "", "tls.key": ""}
        }));
        assert_complete(
            &template(&environment(&document, &manifest, &path, 70)?, "hosts")?,
            &secrets,
        )?;
        println!(
            "platform {project}: {}",
            path.file_name().unwrap().to_string_lossy()
        );
    }
    println!("prekind platform renders elapsed {:?}", started.elapsed());
    Ok(())
}

#[test]
fn prekind_install_renders() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    assert!(
        render
            .objects
            .iter()
            .all(|(_, object)| object["kind"].is_string())
    );
    assert_eq!(render.documents.len(), 2);
    assert_eq!(render.fixtures.releases.len(), 2);
    assert_eq!(render.cluster.org, "test");
    Ok(())
}
