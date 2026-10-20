//! The real HTTP connection guest and its in-cluster recording peer.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use anyhow::ensure;
use serde_json::{Value, json};

use super::{NAMESPACE, ORG, PACKAGE, PASSWORD, PROJECT, Run, USER};
use wamn_catalog::{
    AttachmentType, ComponentDescriptor, ComponentPackageScope, DefinitionHash, ServingComponent,
    ServingManifest, ServingWiring, WiringAttachment, WiringDocument, WorkflowSection,
};

pub const SERVICE: &str = "http-fixture";
pub const SECRET: &str = "apply-http-credentials";
pub const ORIGIN: &str = "http://http-fixture:8080";

pub async fn deploy(run: &Run, repository: &Path) -> anyhow::Result<()> {
    let image = format!("{}-http-fixture:debug", run.cluster);
    run.run(
        "docker",
        &[
            "build",
            "-t",
            &image,
            &repository
                .join("crates/control/lib/tests/support/http-fixture")
                .to_string_lossy(),
        ],
    )
    .await?;
    run.run(
        "kind",
        &["load", "docker-image", "--name", &run.cluster, &image],
    )
    .await?;
    let file = run.work.join("http-fixture.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&json!({
            "apiVersion": "v1", "kind": "List", "items": [
                {
                    "apiVersion": "v1", "kind": "Secret",
                    "metadata": {"name": SECRET, "namespace": NAMESPACE},
                    "stringData": {"credentials.json": json!({
                        PROJECT: {"upstream": json!({
                            "headers": {"authorization": "Bearer fixture-token"}
                        }).to_string()}
                    }).to_string()}
                },
                {
                    "apiVersion": "apps/v1", "kind": "Deployment",
                    "metadata": {"name": SERVICE, "namespace": NAMESPACE},
                    "spec": {"replicas": 1, "selector": {"matchLabels": {"app": SERVICE}},
                        "template": {"metadata": {"labels": {"app": SERVICE}},
                            "spec": {"containers": [{
                                "name": SERVICE, "image": image, "imagePullPolicy": "Never",
                                "ports": [{"containerPort": 8080}],
                                "readinessProbe": {"tcpSocket": {"port": 8080}}
                            }]}}}
                },
                {
                    "apiVersion": "v1", "kind": "Service",
                    "metadata": {"name": SERVICE, "namespace": NAMESPACE},
                    "spec": {"selector": {"app": SERVICE},
                        "ports": [{"port": 8080, "targetPort": 8080}]}
                }
            ]
        }))?,
    )?;
    run.run("kubectl", &["apply", "-f", &file.to_string_lossy()])
        .await?;
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "rollout",
            "status",
            &format!("deployment/{SERVICE}"),
            "--timeout=120s",
        ],
    )
    .await?;
    Ok(())
}

/// Admit the actual debug guest bytes and upload the production artifact layout.
pub async fn component(
    run: &Run,
    repository: &Path,
    registry: &str,
    version: &str,
    name: &str,
) -> anyhow::Result<ComponentDescriptor> {
    use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, Config, ImageLayer};
    use oci_client::manifest::OciImageManifest;
    use oci_client::secrets::RegistryAuth;
    use wamn_engine::component_artifact::{
        component_artifact_config_bytes, component_artifact_layout,
    };

    let target = run.work.join("http-guests");
    run.run(
        "cargo",
        &[
            "build",
            "--locked",
            "--offline",
            "--manifest-path",
            &repository
                .join("apps/platform/no-std/Cargo.toml")
                .to_string_lossy(),
            "--target-dir",
            &target.to_string_lossy(),
            "--target",
            "wasm32-wasip2",
            "-p",
            name,
        ],
    )
    .await?;
    let bytes = std::fs::read(
        target
            .join("wasm32-wasip2/debug")
            .join(format!("{}.wasm", name.replace('-', "_"))),
    )?;
    let template = std::fs::read_to_string(
        repository.join(format!("apps/platform/no-std/{name}/declaration.json.in")),
    )?;
    let declaration = serde_json::from_str(
        &template
            .replace(
                "__TENANT_ID__",
                &wamn_control_registry::project_env_tenant(ORG, PROJECT, "http"),
            )
            .replace("__PACKAGE_ID__", PACKAGE)
            .replace("__PACKAGE_VERSION__", version),
    )?;
    let engine = wamn_engine::build_engine(&[])?;
    let admitted = wamn_engine::component_admission::validate_component_admission(
        &engine,
        &bytes,
        wamn_engine::component_admission::ComponentAdmissionRequest {
            declaration,
            admitted_platform_packages: ["wamn:node".to_owned(), "wamn:connection".to_owned()]
                .into(),
            base_digests: BTreeSet::new(),
        },
    )?;
    let reference = wamn_catalog::component_artifact_reference(
        &format!("{registry}/wamn/components"),
        &admitted.component.component_digest,
    )?;
    let reference = oci_client::Reference::with_tag(
        reference.registry().to_owned(),
        reference.repository().to_owned(),
        reference.tag().to_owned(),
    );
    let config_bytes = component_artifact_config_bytes(&admitted.component);
    let layout = component_artifact_layout(&bytes, &config_bytes);
    let layer = ImageLayer::new(bytes.clone(), layout.layer_media_type().to_owned(), None);
    let config = Config::new(
        config_bytes.clone(),
        layout.config_media_type().to_owned(),
        None,
    );
    let manifest = OciImageManifest::build(std::slice::from_ref(&layer), &config, None);
    oci_client::Client::new(ClientConfig {
        extra_root_certificates: vec![Certificate {
            encoding: CertificateEncoding::Pem,
            data: std::fs::read(run.work.join("ca.crt"))?,
        }],
        read_timeout: Some(Duration::from_secs(30)),
        connect_timeout: Some(Duration::from_secs(30)),
        ..Default::default()
    })
    .push(
        &reference,
        &[layer],
        config,
        &RegistryAuth::Basic(USER.to_owned(), PASSWORD.to_owned()),
        Some(manifest),
    )
    .await?;
    Ok(ComponentDescriptor::new(
        admitted.component,
        admitted.connections,
    ))
}

pub fn wiring() -> anyhow::Result<WiringDocument> {
    Ok(serde_json::from_value(json!({
        "format-version": "0.1", "wiring-id": "http-send", "version": 1,
        "entry": "input",
        "nodes": {
            "input": {"component": "transform", "interface-version": "0.1.0",
                "operation": "wamn:node/handler@0.1.0", "params": {"pointer": ""}},
            "send": {"component": "http-request", "interface-version": "0.1.0",
                "operation": "wamn:node/async-handler@0.1.0",
                "params": {"requirement": "upstream", "method": "POST", "path-and-query": "/reuse"},
                "terminal": "respond"}
        },
        "edges": [{"from": "input", "to": "send"}]
    }))?)
}

pub fn release(
    h: &[u8],
    version: &str,
    components: Vec<ComponentDescriptor>,
) -> anyhow::Result<Vec<u8>> {
    let scope = ComponentPackageScope {
        tenant_id: wamn_control_registry::project_env_tenant(ORG, PROJECT, "http"),
        package_id: PACKAGE.to_owned(),
        package_version: version.to_owned(),
    };
    let facts: Vec<_> = components
        .iter()
        .cloned()
        .map(|descriptor| descriptor.into_admitted(scope.clone()).component)
        .collect();
    let wiring = wiring()?;
    wamn_control::author_wiring::gate_wiring_document(&wiring, &scope, &facts)?;
    let (mut manifest, _) = ServingManifest::from_canonical_bytes(h)?;
    for (descriptor, fact) in components.into_iter().zip(&facts) {
        manifest
            .components
            .insert(ServingComponent::project(fact, descriptor.connections)?);
    }
    let definition = json!({
        "id": "http-send", "type": "http",
        "route": {"method": "POST", "path": "/http/send"},
        "run-deadline-ms": 30000
    });
    manifest.workflow = WorkflowSection {
        wirings: [ServingWiring::new(PACKAGE, wiring)].into(),
        attachments: [(
            "http-send".to_owned(),
            WiringAttachment {
                type_: AttachmentType::Http,
                package_id: PACKAGE.to_owned(),
                wiring_id: "http-send".to_owned(),
                wiring_version: 1,
                definition_hash: DefinitionHash::parse(
                    wamn_execution_contract::canonical_json_sha256(&definition),
                )?,
                definition,
                auth_policy: json!({"modes": ["none"]}),
                registered_operation: None,
            },
        )]
        .into(),
        ..WorkflowSection::default()
    };
    let bytes = manifest.canonical_bytes();
    ServingManifest::from_canonical_bytes(&bytes)?;
    Ok(bytes)
}

pub fn assert_observed_request(status: u16, body: &str, log: &str) -> anyhow::Result<()> {
    ensure!(status == 200, "route returned {status}: {body}");
    let response: Value = serde_json::from_str(body)?;
    ensure!(response["status"] == 200, "guest response: {response}");
    ensure!(
        response["body"] == json!({"id": 1}),
        "guest body: {response}"
    );
    let normalized = log.replace("\r\n", "\n");
    let requests: Vec<_> = normalized.split("\n\n").filter(|s| !s.is_empty()).collect();
    ensure!(
        requests.len() == 1,
        "fixture saw {} requests",
        requests.len()
    );
    ensure!(
        requests[0].lines().next() == Some("POST /reuse HTTP/1.1"),
        "fixture received the wrong target or protocol"
    );
    let headers: Vec<_> = requests[0]
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .collect();
    let authorization: Vec<_> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.trim())
        .collect();
    ensure!(
        authorization == ["Bearer fixture-token"],
        "wrong credential reached the wire"
    );
    ensure!(
        headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("idempotency-key")
                && value.trim().ends_with(":send:0")),
        "effect omitted its delivery idempotency key"
    );
    Ok(())
}
