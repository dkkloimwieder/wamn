//! The environment kind fixture rendered without a cluster or a running host.

use super::*;
use wamn_gate_harness::render_check::{
    assert_complete, credential_secrets, environment as render_values, operator_prerequisites,
    prerequisite, template,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

fn manifest(bytes: &[u8]) -> anyhow::Result<wamn_catalog::ServingManifest> {
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(bytes)?;
    Ok(manifest)
}

fn fixture_secrets(document: &EnvironmentDocument, event_secret: Value) -> Vec<Value> {
    let mut secrets = credential_secrets(&document.triple(), NAMESPACE);
    secrets.extend(operator_prerequisites(NAMESPACE));
    // scenario() emits apply-registry before installing any environment.
    secrets.push(prerequisite(
        "apply-registry",
        &["config.json", "ca.crt"],
        NAMESPACE,
    ));
    secrets.push(event_secret);
    secrets
}

#[test]
fn prekind_environment_renders() -> anyhow::Result<()> {
    let started = Instant::now();
    let work = ScratchRoot::create()?;
    let [a, b, h, http] = fixture_releases()?;
    let base = fixture_host_group("registry.invalid");
    std::fs::write(
        work.path().join("host-group.yaml"),
        serde_yaml::to_string(&base)?,
    )?;
    for (name, env, bytes, drain) in [
        ("dev A", "dev", &a, 70),
        ("dev B", "dev", &b, 70),
        ("rollback A", "dev", &a, 70),
        ("dev short-drain", "dev", &a, 30),
        ("qa", "qa", &a, 70),
        ("HTTP fixture", "http", &http, 70),
        ("east H", "east", &h, 70),
        ("west H", "west", &h, 70),
    ] {
        let manifest = manifest(bytes)?;
        let document = fixture_document(
            env,
            &format!("{env}.orders.example"),
            manifest.digest().as_str(),
        );
        let base = serde_yaml::from_slice(&std::fs::read(work.path().join("host-group.yaml"))?)?;
        let (group, event_secret) = event_overlay(&document, base, "nats://events:4222")?;
        let path = work.path().join(format!("{env}-event-host-group.yaml"));
        std::fs::write(&path, serde_yaml::to_string(&group)?)?;
        let selected = if name == "rollback A" {
            rollback_host_group(work.path())
        } else {
            path
        };
        let values = render_values(&document, &manifest, &selected, drain)?;
        let documents = template(&values, NAMESPACE)?;
        assert_complete(&documents, &fixture_secrets(&document, event_secret))
            .with_context(|| name.to_owned())?;
        println!(
            "{name}: host-group.yaml + {}",
            selected.file_name().unwrap().to_string_lossy()
        );
    }
    // Both connectors share integration.k and its session/event host overlay.
    for connector in ["rest_connector", "netsuite"] {
        let document = integration::fixture_document(connector);
        let group = integration::event_host_values(
            base.clone(),
            "runtime",
            "nats://events:4222",
            "https://integration-session",
            "render",
        )?;
        let path = work.path().join("integration-host-group.yaml");
        std::fs::write(&path, serde_yaml::to_string(&group)?)?;
        let mut release: Value = serde_json::from_slice(&a)?;
        release["components"] = json!([]);
        release["release"]["packages"] = serde_json::to_value(
            document
                .applications
                .values()
                .flat_map(|application| &application.packages)
                .map(|package| json!({"package-id":package, "package-version":"2.1.0"}))
                .collect::<Vec<_>>(),
        )?;
        let release = manifest(&wamn_execution_contract::canonical_json_bytes(&release))?;
        let values = render_values(&document, &release, &path, 70)?;
        let event_secret = integration::event_secret("render", "{}");
        assert_complete(
            &template(&values, NAMESPACE)?,
            &fixture_secrets(&document, event_secret),
        )
        .with_context(|| connector.to_owned())?;
        println!(
            "{connector}: integration.k + integration-host-group.yaml with session and events"
        );
    }
    println!(
        "prekind environment renders elapsed {:?}",
        started.elapsed()
    );
    Ok(())
}

#[test]
fn prekind_rejects_old_missing_conn_reader() -> anyhow::Result<()> {
    let work = ScratchRoot::create()?;
    let manifest = manifest(&fixture_releases()?[0])?;
    let document = fixture_document("dev", "dev.orders.example", manifest.digest().as_str());
    let (group, event_secret) = event_overlay(
        &document,
        fixture_host_group("registry.invalid"),
        "nats://events:4222",
    )?;
    let path = rollback_host_group(work.path());
    std::fs::write(&path, serde_yaml::to_string(&group)?)?;
    let documents = template(&render_values(&document, &manifest, &path, 70)?, NAMESPACE)?;
    let mut secrets = fixture_secrets(&document, event_secret);
    assert_complete(&documents, &secrets)?;
    let missing = wamn_control_provision::workload_secret_name(
        WorkloadRoleFamily::ConnectionReader,
        ORG,
        PROJECT,
        "dev",
    );
    secrets.retain(|secret| secret["metadata"]["name"] != missing);
    let error = assert_complete(&documents, &secrets).unwrap_err();
    assert!(
        error
            .to_string()
            .contains(&format!("missing Secret {NAMESPACE}/{missing}")),
        "{error:#}"
    );
    println!("0.1.121 proof: {error:#}");
    Ok(())
}

#[test]
fn prekind_rejects_old_rollback_without_events() -> anyhow::Result<()> {
    let work = ScratchRoot::create()?;
    let manifest = manifest(&fixture_releases()?[0])?;
    let document = fixture_document("dev", "dev.orders.example", manifest.digest().as_str());
    let (mut group, event_secret) = event_overlay(
        &document,
        fixture_host_group("registry.invalid"),
        "nats://events:4222",
    )?;
    let path = rollback_host_group(work.path());
    std::fs::write(&path, serde_yaml::to_string(&group)?)?;
    let secrets = fixture_secrets(&document, event_secret);
    assert_complete(
        &template(&render_values(&document, &manifest, &path, 70)?, NAMESPACE)?,
        &secrets,
    )?;
    // A12 selected the original base group, without the event overlay.
    group["env"]
        .as_array_mut()
        .context("environment")?
        .retain(|var| var["name"] != "WAMN_MAT_NATS_BINDING_FILE");
    std::fs::write(&path, serde_yaml::to_string(&group)?)?;
    let error = assert_complete(
        &template(&render_values(&document, &manifest, &path, 70)?, NAMESPACE)?,
        &secrets,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("lacks WAMN_MAT_NATS_BINDING_FILE"),
        "{error:#}"
    );
    println!("0.1.117 proof: {error:#}");
    Ok(())
}

fn pat_release(project: &str) -> anyhow::Result<()> {
    use wamn_control::release_chart::ValuesInput;
    use wamn_test_infrastructure::rendering::{
        EventIdentity, HostRoleSecret, HostValuesInput, object_store_overlay, render_host_values,
    };
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let scope = Triple::new("acme", project, "dev");
    let secrets = credential_secrets(&scope, NAMESPACE);
    let guest = wamn_control_provision::workload_secret_name(
        WorkloadRoleFamily::App,
        "acme",
        project,
        "dev",
    );
    let roles = wamn_control_provision::host_credentials::HOST_CREDENTIALS
        .iter()
        .map(|credential| credential.family)
        .filter(|family| {
            !matches!(
                family,
                WorkloadRoleFamily::App | WorkloadRoleFamily::RegistryReader
            )
        })
        .map(|family| HostRoleSecret {
            family,
            name: wamn_control_provision::workload_secret_name(family, "acme", project, "dev"),
        })
        .collect();
    let event = EventIdentity {
        org: "acme".into(),
        project: project.into(),
        environment: "dev".into(),
    };
    let manifest = manifest(&fixture_releases()?[2])?;
    // Use the application cases' PAT files and fixture object-store mount.
    let mut overlay = wamn_test_infrastructure::rendering::overlay_values(
        &std::fs::read_to_string(
            repository.join(format!("deploy/platform/values-host-{project}-pat.yaml")),
        )?,
        &event,
    )?;
    let object_store_secret_name =
        (project == "wms").then(|| format!("wamn-object-store-credentials-acme--{project}--dev"));
    let mut secrets = secrets;
    if let Some(name) = &object_store_secret_name {
        overlay = object_store_overlay(&overlay, name)?;
        secrets.push(prerequisite(name, &["credentials.json"], NAMESPACE));
    }
    let rendered = render_host_values(
        &std::fs::read_to_string(repository.join("deploy/platform/values-host-default.yaml"))?,
        &overlay,
        &HostValuesInput {
            namespace: NAMESPACE.into(),
            host_tag: "prekind".into(),
            replicas: 3,
            component_artifact_base: "registry.invalid/wamn/components".into(),
            release_artifact_base: "registry.invalid/wamn/releases".into(),
            manifest_digest: manifest.digest().as_str().into(),
            nats_url: "nats://events:4222".into(),
            stream_replicas: 1,
            dup_window_secs: 120,
            event,
            guest_secret_name: guest,
            role_secrets: roles,
            object_store_secret_name,
        },
    )?;
    let overlay = wamn_gate_harness::session_issuer::adjust_host(
        &rendered.overlay,
        "https://session",
        "render",
    )?;
    let roles = wamn_gate_harness::render_check::fixture_roles(&manifest, "acme", project, "dev");
    let values = wamn_gate_harness::case_release::render(&ValuesInput {
        org: "acme".into(),
        project: project.into(),
        env: "dev".into(),
        manifest_digest: manifest.digest().as_str().into(),
        artifact_base: "registry.invalid/wamn/releases".into(),
        route_host: format!("{project}.example"),
        applications: [(
            project.into(),
            wamn_catalog::ApplicationDefinition {
                connections: std::collections::BTreeMap::new(),
                packages: manifest
                    .release
                    .packages
                    .iter()
                    .map(|package| package.package_id().to_owned())
                    .collect(),
            },
        )]
        .into(),
        roles,
        drain_bound_seconds: 70,
        actor: "prekind".into(),
        host_group: wamn_gate_harness::case_release::host_group(&overlay)?,
    })?;
    secrets.extend(operator_prerequisites(NAMESPACE));
    // platform fixture startup emits these event and registry Secrets before
    // either application installs its release. The PAT files document the keys.
    secrets.extend([
        prerequisite("wamn-event-nats", &["username", "password"], NAMESPACE),
        prerequisite("wamn-materializer-nats", &["binding.json"], NAMESPACE),
        prerequisite("wamn-registry-pull", &[".dockerconfigjson"], NAMESPACE),
    ]);
    assert_complete(&template(&values, NAMESPACE)?, &secrets)?;
    println!("{project}: PAT overlay -> case_release::render -> release chart");
    Ok(())
}

#[test]
fn prekind_receiving_pat_renders() -> anyhow::Result<()> {
    pat_release("receiving")
}

#[test]
fn prekind_wms_pat_renders() -> anyhow::Result<()> {
    pat_release("wms")
}
