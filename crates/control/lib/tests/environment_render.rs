//! The environment kind fixture rendered without a cluster or a running host.

use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;
use wamn_control::environment::document::EnvironmentDocument;
use wamn_control_provision::WorkloadRoleFamily;
use wamn_control_registry::Triple;
#[path = "support/release_documents.rs"]
pub mod release_documents;
use release_documents::workflow as integration;
use release_documents::{
    MANIFEST, MANIFEST_B, NAMESPACE, ORG, PROJECT, environment_releases as fixture_releases,
    event_overlay, fixture_document, fixture_host_group, release, rollback_host_group,
};
use wamn_gate_harness::render_check::{
    assert_complete, credential_secrets, environment as render_values, operator_prerequisites,
    prerequisite, template,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

#[test]
fn prekind_deploy_connections_cover_the_wms_environment() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let document =
        EnvironmentDocument::compile(&repository.join("deploy/gcp/environments/dkk--wms--dev.k"))?;
    let directory = repository.join("deploy/gcp/connections");
    for name in document
        .applications
        .values()
        .flat_map(|app| app.connections.values())
    {
        let connection: Value =
            serde_json::from_slice(&std::fs::read(directory.join(format!("{name}.json")))?)?;
        let integration: Value = serde_json::from_slice(&std::fs::read(
            directory.join(format!("{name}.integration.json")),
        )?)?;
        assert_eq!(connection["name"], name.as_str());
        assert_eq!(connection["project"], document.project);
        assert_eq!(connection["env"], document.env);
        assert!(document.integrations.contains_key(name));
        wamn_catalog::RequirementType::for_integration(
            integration["integration_type"]
                .as_str()
                .context("integration type")?,
        )
        .context("supported host integration")?
        .check_definition(&connection["parameters"])
        .map_err(anyhow::Error::msg)?;
        if let Some(system) = integration["external_system"].as_str() {
            let external: Value = serde_json::from_slice(&std::fs::read(
                directory.join(format!("{system}.external-system.json")),
            )?)?;
            assert_eq!(external["name"], system);
            assert_eq!(external["project"], document.project);
        }
    }
    Ok(())
}

fn manifest(bytes: &[u8]) -> anyhow::Result<wamn_catalog::ServingManifest> {
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(bytes)?;
    Ok(manifest)
}

fn fixture_secrets(document: &EnvironmentDocument) -> Vec<Value> {
    let mut secrets = credential_secrets(&document.triple(), NAMESPACE);
    secrets.extend(operator_prerequisites(NAMESPACE));
    // scenario() emits apply-registry before installing any environment.
    secrets.push(prerequisite(
        "apply-registry",
        &["config.json", "ca.crt"],
        NAMESPACE,
    ));
    let mut oauth = prerequisite("wamn-oauth", &["key"], NAMESPACE);
    oauth["stringData"]["key"] = "o".repeat(32).into();
    secrets.push(oauth);
    // cert-manager issues the environment's NATS certificate, which the host
    // mounts as its data certificate (docs/plan/alignment.md R5).
    let env_key =
        wamn_control_registry::project_env_key(&document.org, &document.project, &document.env);
    secrets.push(prerequisite(
        &format!("wamn-nats-{env_key}"),
        &["ca.crt", "tls.crt", "tls.key"],
        NAMESPACE,
    ));
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
        let group = event_overlay(&document, base)?;
        let path = work.path().join(format!("{env}-event-host-group.yaml"));
        std::fs::write(&path, serde_yaml::to_string(&group)?)?;
        let selected = if name == "rollback A" {
            rollback_host_group(work.path())
        } else {
            path
        };
        let values = render_values(&document, &manifest, &selected, drain)?;
        let documents = template(&values, NAMESPACE)?;
        assert_complete(&documents, &fixture_secrets(&document))
            .with_context(|| name.to_owned())?;
        println!(
            "{name}: host-group.yaml + {}",
            selected.file_name().unwrap().to_string_lossy()
        );
    }
    // Both connectors share integration.k and its session/event host overlay.
    for connector in ["rest_connector", "netsuite"] {
        let document = integration::fixture_document(connector);
        let group =
            integration::event_host_values(base.clone(), "https://integration-session", "render")?;
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
        assert_complete(&template(&values, NAMESPACE)?, &fixture_secrets(&document))
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
    let group = event_overlay(&document, fixture_host_group("registry.invalid"))?;
    let path = rollback_host_group(work.path());
    std::fs::write(&path, serde_yaml::to_string(&group)?)?;
    let documents = template(&render_values(&document, &manifest, &path, 70)?, NAMESPACE)?;
    let mut secrets = fixture_secrets(&document);
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
    // platform fixture startup emits the registry Secret, and cert-manager the
    // environment's NATS certificate, before either application installs its
    // release.
    secrets.extend([
        prerequisite("wamn-registry-pull", &[".dockerconfigjson"], NAMESPACE),
        prerequisite(
            &format!("wamn-nats-acme--{project}--dev"),
            &["ca.crt", "tls.crt", "tls.key"],
            NAMESPACE,
        ),
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

#[test]
fn the_fixture_releases_are_canonical_manifests() {
    for template in [MANIFEST, MANIFEST_B] {
        release(template, "2.1.0", None).expect("a fixture release");
    }
    let [_, _, h, bytes] = fixture_releases().unwrap();
    let (h, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&h).unwrap();
    let device = release_documents::fixture_device().unwrap();
    let served = &h
        .components
        .first()
        .expect("the device component")
        .operations["edge-device:sample/read@2.1.0"];
    assert!(
        served.carries(&device.operations["edge-device:sample/read@2.1.0"]),
        "release H carries the admitted device operation"
    );

    let (http, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes).unwrap();
    assert_eq!(
        http.components
            .iter()
            .map(|component| component.descriptor.component.as_str())
            .collect::<Vec<_>>(),
        ["device", "http-page", "http-request", "transform"]
    );
    assert_eq!(http.routes, h.routes);
    assert_eq!(http.attachments, h.attachments);
    assert!(
        serde_json::to_value(&http)
            .unwrap()
            .get("workflow")
            .is_none()
    );
    let scheduled = serde_json::to_value(http_connection::scheduled_wiring().unwrap()).unwrap();
    assert_eq!(scheduled["nodes"].as_object().unwrap().len(), 2);
    assert!(
        scheduled["nodes"]
            .as_object()
            .unwrap()
            .values()
            .all(|node| node.get("terminal").is_none())
    );
    let request = http
        .components
        .iter()
        .find(|component| component.descriptor.component == "http-request")
        .unwrap();
    assert_eq!(request.descriptor.connections.len(), 4);
    let signed = serde_json::to_value(http_connection::signed_wiring().unwrap()).unwrap();
    assert_eq!(
        signed["nodes"]["send"]["params"],
        json!({
            "requirement": http_connection::OUTBOUND_HANDLE, "method": "POST", "path-and-query": "/signed"
        })
    );
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains(http_connection::INBOUND_SECRET));
    assert!(!text.contains(http_connection::OUTBOUND_SECRET));
}

#[path = "support/environment_checks.rs"]
pub mod environment_checks;
use environment_checks::{
    Kept, cluster_name, keep_on_failure, require_phases, reuse_checks, select_checks,
};
use wamn_control::release_chart::ImageSet;
const CHECKS: [environment_checks::Check<()>; 7] = environment_checks::checks([(); 7]);

#[test]
fn check_selection_trims_and_deduplicates() -> anyhow::Result<()> {
    let selected = select_checks(&CHECKS, " 4 netsuite,4 show reapply, 4 netsuite ")?;
    assert_eq!(
        selected.iter().map(|check| check.name).collect::<Vec<_>>(),
        ["4 netsuite", "4 show reapply"]
    );
    for names in ["", " ", "4 netsuite,", "unknown"] {
        let error = select_checks(&CHECKS, names)
            .err()
            .context("selection must fail")?
            .to_string();
        for check in &CHECKS {
            assert!(error.contains(check.name), "{error}");
        }
    }
    Ok(())
}

#[test]
fn reuse_requires_both_inputs() -> anyhow::Result<()> {
    assert!(reuse_checks(&CHECKS, None, None)?.is_none());
    assert!(reuse_checks(&CHECKS, Some("kept.json"), None).is_err());
    assert!(reuse_checks(&CHECKS, None, Some("4 netsuite")).is_err());
    assert!(reuse_checks(&CHECKS, Some(""), Some("4 netsuite")).is_err());
    assert!(reuse_checks(&CHECKS, Some("kept.json"), Some("4 netsuite"))?.is_some());
    Ok(())
}

#[test]
fn checks_refuse_missing_phases() -> anyhow::Result<()> {
    let selected = select_checks(&CHECKS, "4 show reapply")?;
    let mut phases = vec!["environment".into(), "release".into()];
    let error = require_phases(&selected, &phases).unwrap_err().to_string();
    assert!(error.contains("bindings") && error.contains("4 show reapply"));
    phases.push("bindings".into());
    require_phases(&selected, &phases)
}

#[test]
fn kept_metadata_round_trips() -> anyhow::Result<()> {
    let kept = Kept {
        cluster: "wamn-apply-01234567".into(),
        kubeconfig: "/tmp/original/kubeconfig".into(),
        registry: "wamn-apply-01234567-registry".into(),
        work: "/tmp/rerun".into(),
        secret_work: "/tmp/original".into(),
        failed: vec!["4 netsuite".into()],
        phases: vec!["environment".into()],
        images: [("host".into(), "sha256:abc".into())].into(),
        role_inputs: [("http".into(), "sha256:def".into())].into(),
        image_set: Some(ImageSet {
            host: "host@sha256:abc".into(),
            http: "http@sha256:def".into(),
            materializer: "materializer@sha256:123".into(),
        }),
        registry_address: "172.18.0.2:5000".into(),
        release_digest: "sha256:456".into(),
        releases: [("files".into(), b"manifest".to_vec())].into(),
        file_setup_error: None,
        identity_ready: true,
        fixture_source: "abcdef".into(),
        brokers: [("events".into(), "/tmp/original/rest_connector".into())].into(),
        cleanup: "tools/kind-kept-delete '/tmp/rerun/kept.json'".into(),
    };
    assert_eq!(
        kept,
        serde_json::from_slice(&serde_json::to_vec_pretty(&kept)?)?
    );
    Ok(())
}

#[test]
fn keep_policy_accepts_only_never_or_unset() -> anyhow::Result<()> {
    assert!(keep_on_failure(None)?);
    assert!(keep_on_failure(Some(""))?);
    assert!(!keep_on_failure(Some("never"))?);
    let error = keep_on_failure(Some("always")).unwrap_err().to_string();
    assert!(error.contains("WAMN_KIND_KEEP") && error.contains("never"));
    Ok(())
}

#[test]
fn cluster_names_use_the_checkout_path() {
    let path = Path::new("/tmp/checkout");
    let name = cluster_name(path);
    assert_eq!(name, cluster_name(path));
    assert_ne!(name, cluster_name(Path::new("/tmp/other-checkout")));
    assert_eq!(name.len(), "wamn-apply-".len() + 8);
    assert!(name.starts_with("wamn-apply-"));
    assert!(
        name["wamn-apply-".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert_eq!(name, "wamn-apply-69901e25");
}
#[test]
fn host_report_release_requires_component_preload() -> anyhow::Result<()> {
    let device = release_documents::fixture_device()?;
    let bytes = release_documents::host_report_release("2.1.0", &device)?;
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)?;
    assert!(manifest.composed_entry.is_none());
    assert_eq!(wamn_execution_host::synchronous_route_count(&manifest), 1);
    let component = manifest.components.first().context("the served device")?;
    assert_eq!(
        component.descriptor.component_digest,
        device.component_digest
    );
    assert_eq!(component.descriptor.component, "device");
    assert!(manifest.routes.iter().all(|route| {
        route.component == component.descriptor.component
            && component.operations.contains_key(&route.operation)
    }));
    let roles = wamn_control::release_chart::roles(&manifest, ORG, PROJECT, "broken");
    assert!(
        roles
            .iter()
            .all(|role| role.name != wamn_control::release_chart::RoleName::Http),
        "an http role needs a composed entry that the preload would skip"
    );
    Ok(())
}
mod http_connection {
    use super::*;
    use release_documents::PACKAGE;
    pub use release_documents::http::{
        API_KEY_HANDLE, AWS_HANDLE, INBOUND_SECRET, OUTBOUND_HANDLE, OUTBOUND_SECRET,
        bindings_match, credential_secret, paged_wiring, scheduled_wiring, signed_wiring,
    };
    const NETSUITE_PRIVATE_KEY: &str = include_str!("support/http-fixture/fixtures/p256.pem");
    #[test]
    fn scheduled_credentials_stay_within_the_environment_secret_grant() {
        use wamn_credential_store::kubernetes::{secret_name, service_account_name};

        let source_secret = secret_name(ORG, PROJECT, "http");
        let values = json!({"items":[{
            "metadata":{"name":source_secret,"namespace":NAMESPACE},
            "stringData":{
                "00000000-0000-4000-8000-000000000001":r#"{"username":"fixture","password":"fixture-password"}"#,
                "00000000-0000-4000-8000-000000000002":json!({"private_key":NETSUITE_PRIVATE_KEY}).to_string(),
            },
        }]});
        let mut references = serde_json::Map::new();
        for (name, version) in [
            ("rest-fixture", "00000000-0000-4000-8000-000000000001"),
            ("netsuite", "00000000-0000-4000-8000-000000000002"),
        ] {
            references.insert(
                name.into(),
                json!({"store":"k8s","reference":source_secret,"version":version}),
            );
        }
        let secret = credential_secret("integration", &values, &mut references).unwrap();
        let grants = wamn_control::environment::ensure::credential_access(
            &service_account_name(ORG, PROJECT, "integration"),
            &secret_name(ORG, PROJECT, "integration"),
        );
        assert_eq!(secret["metadata"]["namespace"], NAMESPACE);
        for (name, reference) in &references {
            assert_eq!(
                reference["reference"],
                grants[1].1["rules"][0]["resourceNames"][0]
            );
            assert_eq!(reference["reference"], secret["metadata"]["name"]);
            let version = reference["version"].as_str().unwrap();
            let material: Value =
                serde_json::from_str(secret["stringData"][version].as_str().unwrap()).unwrap();
            assert_eq!(
                material,
                if name == "netsuite" {
                    json!({"private_key":NETSUITE_PRIVATE_KEY})
                } else {
                    json!({"username":"fixture","password":"fixture-password"})
                }
            );
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn release_bindings_accept_shared_names_across_components_and_refuse_wrong_scope() {
            let digest = format!("sha256:{}", "a".repeat(64));
            let mut document = super::super::release_documents::fixture_document(
                "http",
                "http.orders.example",
                &digest,
            );
            let handles = ["upstream", OUTBOUND_HANDLE, API_KEY_HANDLE, AWS_HANDLE];
            document.applications.get_mut(PACKAGE).unwrap().connections = handles
                .iter()
                .map(|handle| (handle.to_string(), handle.to_string()))
                .collect();
            let mut rows = json!({"versions": [], "bindings": []});
            for handle in handles {
                rows["versions"].as_array_mut().unwrap().push(json!({
                    "manifest_digest": digest, "connection_id": handle, "version": 1
                }));
                for component in if handle == "upstream" {
                    ["http-request", "http-page"].as_slice()
                } else {
                    ["http-request"].as_slice()
                } {
                    rows["bindings"].as_array_mut().unwrap().push(json!({
                        "manifest_digest": digest, "component_digest": component,
                        "package_id": "http_fixture", "name": handle,
                        "connection_id": handle, "mode": "write", "integration_name": handle
                    }));
                }
            }
            assert!(bindings_match(&rows, &document, &digest));
            for (field, value) in [
                ("package_id", "other_fixture"),
                ("manifest_digest", "other-release"),
                ("mode", "read"),
                ("integration_name", "other-integration"),
            ] {
                let mut wrong = rows.clone();
                wrong["bindings"][0][field] = value.into();
                assert!(!bindings_match(&wrong, &document, &digest), "{field}");
            }
            rows["bindings"].as_array_mut().unwrap().remove(0);
            assert!(!bindings_match(&rows, &document, &digest));
        }

        #[test]
        fn page_items_pointer_selects_the_peer_response_array() {
            let graph = serde_json::to_value(paged_wiring().unwrap()).unwrap();
            let pointer = graph["nodes"]["send"]["params"]["items"].as_str().unwrap();
            let response = json!({"items": [{"id":"one","name":"First"}]});
            assert_eq!(response.pointer(pointer), Some(&response["items"]));
        }
    }
}
