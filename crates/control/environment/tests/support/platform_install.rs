//! Shared install render and its offline inputs.
use super::control;
use anyhow::Context as _;
use control::environment::document::EnvironmentDocument;
use control::platform::document::ClusterDocument;
use control::platform::render::{Fixtures, install};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug)]
pub(super) struct Render {
    _work: wamn_test_infrastructure::scratch::ScratchRoot,
    pub cluster: ClusterDocument,
    pub documents: Vec<(PathBuf, EnvironmentDocument)>,
    pub fixtures: Fixtures,
    pub owner_inputs: Vec<Value>,
    pub objects: Vec<(&'static str, Value)>,
}

pub(super) fn render() -> anyhow::Result<&'static Render> {
    static RENDER: OnceLock<Result<Render, String>> = OnceLock::new();
    RENDER
        .get_or_init(|| build().map_err(|error| format!("{error:#}")))
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))
}
fn environment_key(environment: &EnvironmentDocument) -> String {
    wamn_control_registry::project_env_key(&environment.org, &environment.project, &environment.env)
}
const CERTIFICATE: &str = "-----BEGIN CERTIFICATE-----\nMIIBgTCCASagAwIBAgIBATAKBggqhkjOPQQDAjAeMRwwGgYDVQQDDBNmaXh0dXJl\nLmV4YW1wbGUuY29tMCAXDTI2MTEwMjEyMzgxNloYDzIxMjYxMDA5MTIzODE2WjAe\nMRwwGgYDVQQDDBNmaXh0dXJlLmV4YW1wbGUuY29tMFkwEwYHKoZIzj0CAQYIKoZI\nzj0DAQcDQgAEwothT2JuPjLNlLo2LgOAxv8TwQMunnkNT6sRbiysY2x4+uwQZwAd\nW6EI0ybWuVz4mh9IJzvE7uG4K+JWyfnD1KNTMFEwHQYDVR0OBBYEFAdEfAoyn/sb\nu8NfR2LI9O3aX5RAMB8GA1UdIwQYMBaAFAdEfAoyn/sbu8NfR2LI9O3aX5RAMA8G\nA1UdEwEB/wQFMAMBAf8wCgYIKoZIzj0EAwIDSQAwRgIhAP376NiYdu2wfzDjSFTL\nrUBTkwITOUHqIMv4XjQALrwmAiEAzeBS6qodhVMRPVjAwGf8SNpylD5gyy7jWy36\nE9Trpas=\n-----END CERTIFICATE-----\n";

fn fixture_release(package: &str) -> anyhow::Result<wamn_catalog::ServingManifest> {
    let digest = format!("sha256:{}", "1".repeat(64));
    let operation = format!("{}:sample/read@2.1.0", package.replace('_', "-"));
    let mut descriptor = serde_json::to_value(wamn_catalog::ComponentDescriptor::named(
        "device", "0.1", &digest,
    ))?;
    descriptor["operations"] = json!({operation.as_str():{"registered-operation":wamn_catalog::operation_token(package,"2.1.0","sample","read"),"input-ports":[],"output-ports":[],"parameters":[]}});
    let descriptor: wamn_catalog::ComponentDescriptor = serde_json::from_value(descriptor)?;
    let admitted = descriptor
        .into_admitted(wamn_catalog::ComponentPackageScope {
            env_key: String::new(),
            package_id: package.into(),
            package_version: "2.1.0".into(),
        })
        .component;
    let component = wamn_catalog::ServingComponent::project(&admitted, Vec::new())?;
    let value = json!({"format-version":11,"release":{"packages":[{"package-id":package,"package-version":"2.1.0"}]},"components":[component],
            "attachments":{"sample-read-http":{"auth-policy":{"modes":["session"]},"component":"device","definition":{"id":"sample-read-http","route":{"method":"GET","path":"/sample/read"},"run-deadline-ms":30000,"type":"http"},"definition-hash":format!("sha256:{}","5".repeat(64)),"operation":operation,"package-id":package,"type":"http"}},
            "routes":[{"component":"device","operation":operation,"package-id":package,"type":"get"}]});
    Ok(wamn_catalog::ServingManifest::from_canonical_bytes(
        &wamn_execution_contract::canonical_json_bytes(&value),
    )?
    .0)
}

pub(super) fn build() -> anyhow::Result<Render> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let cluster = ClusterDocument::from_json(&serde_json::to_vec(&json!({
        "provider":"kind", "project_id":"", "zone":"", "cluster_name":"render",
        "org":"test", "owner_email":"owner@example.com", "registry":"registry.invalid/wamn",
        "buckets":{"web":"fixture-web", "otel":"", "state":""},
        "route_hosts":{"control":"control.example.com"}, "resend_from":"sender@example.com"
    }))?)?;
    let work = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    // platform_apply_kind writes the WMS document first as a-unqualified-wms.k.
    let documents = ["wms", "receiving"]
        .into_iter()
        .map(|project| {
            let mut environment = EnvironmentDocument::compile(
                &repository.join(format!("deploy/gcp/environments/dkk--{project}--dev.k")),
            )?;
            environment.org.clone_from(&cluster.org);
            environment.route_host = format!("{project}.example.com");
            let name = if project == "wms" {
                "a-unqualified-wms.k"
            } else {
                "test--receiving--dev.k"
            };
            let path = work.path().join(name);
            std::fs::write(&path, environment.to_kcl())?;
            Ok((path.clone(), EnvironmentDocument::compile(&path)?))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let image = format!(
        "registry.invalid/wamn/fixture:render@sha256:{}",
        "1".repeat(64)
    );
    wamn_release_control::release_chart::stamp(
        &repository.join("deploy/platform/release"),
        &wamn_release_control::release_chart::ImageSet {
            host: image.clone(),
            http: image.clone(),
            materializer: image.clone(),
        },
        Some(&wamn_release_control::release_chart::QualificationImages {
            runner: image.clone(),
            gates: image.clone(),
            identity: image,
        }),
        &work.path().join("release-chart"),
    )?;
    let chart_file = work.path().join("release-chart/values.yaml");
    let mut chart: Value = serde_yaml::from_slice(&std::fs::read(&chart_file)?)?;
    chart["platform"]["edge"]["image"] = json!(format!(
        "registry.invalid/wamn/edge@sha256:{}",
        "1".repeat(64)
    ));
    std::fs::write(&chart_file, serde_yaml::to_string(&chart)?)?;
    let mut fixtures = Fixtures {
            identity_tls: json!({"data": {"ca.crt": "Zml4dHVyZS1jYQ=="}}),
            broker_authorization_exists: false,
            drain_bound_seconds:70,
            actor:"platform render".into(),
            edge_certificate:base64::Engine::encode(&base64::engine::general_purpose::STANDARD,CERTIFICATE),
            proxy_uid:"00000000-0000-0000-0000-000000000002".into(),
            https_results:Vec::new(),
            web_hosts:["control.example.com".into()].into(),
            control_operator:json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"operator-test","namespace":"identity","resourceVersion":"1"},"type":"kubernetes.io/tls","data":{"ca.crt":"Zml4dHVyZS1jYQ==","tls.crt":"Zml4dHVyZS1jcnQ=","tls.key":"Zml4dHVyZS1rZXk="}}),
            platform_ui_files:BTreeMap::new(),
            cdc_credentials:BTreeMap::new(),
            host_image:format!("registry.invalid/wamn/host@sha256:{}","1".repeat(64)),
            releases:BTreeMap::new(),
            web_clients:BTreeMap::new(),
            applied:BTreeMap::new(),
            oauth_key:[0x41;32],
            oauth_exists:false,
            registry: wamn_package_artifact::PackageRegistry {artifact_base:"registry.invalid/wamn/packages".into(), registry_auth_file:work.path().join("registry.json"), insecure_registry:true, oci_ca_paths:Vec::new()},
            identity_database: json!({"data":{"url":"cG9zdGdyZXNxbDovL2lkZW50aXR5OmZpeHR1cmVAZGF0YWJhc2Uvd2Ftbl9zeXN0ZW0="}}),
            identity_image: format!("registry.invalid/wamn/identity@sha256:{}", "1".repeat(64)),
            postgres_image: "postgres:18".into(),
            environment_urls: BTreeMap::new(),
            ctl_nats: json!({"data":{"ca.crt":"Zml4dHVyZS1jYQ==", "tls.crt":"Zml4dHVyZS1jcnQ=", "tls.key":"Zml4dHVyZS1rZXk="}}),
            directory: work.path().to_owned(),
            apply_url: "postgresql://wamn_apply:fixture@database/wamn_system".into(),
            postgres_superuser: json!({"data":{"password":"cGFzc3dvcmQ="}}),
            runner_image: format!("registry.invalid/wamn/runner@sha256:{}", "1".repeat(64)),
            job_uid: "00000000-0000-0000-0000-000000000001".into(),
            emitted: [
                ("platform-system".into(), json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
                    "metadata":{"name":"wamn-apply","namespace":"platform","labels":{"app.kubernetes.io/name":"wamn-apply"}},
                    "stringData":{"url":"postgresql://wamn_apply:fixture@database/wamn_system"}})),
                ("platform-identity-issuer".into(), json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
                    "metadata":{"name":"wamn-identity-db","namespace":"identity","labels":{"app.kubernetes.io/name":"wamn-identity","app.kubernetes.io/managed-by":"wamn"}},
                    "stringData":{"url":"postgresql://identity:fixture@database/wamn_system"}})),
            ].into(),
            outputs: [
                ("platform-session-key-state".into(),r#"{"kid":null}"#.into()),
                ("platform-session-key-publish".into(),r#"{"kid":"fixture-kid"}"#.into()),
                ("platform-identity-check".into(),r#"{"keys":[{"kid":"fixture-kid"}]}"#.into()),
            ].into(),
        };
    fixtures
        .outputs
        .insert("platform-owner-state-test".into(), "f\n".into());
    fixtures.platform_ui_files = [
        (
            "chart/Chart.yaml".into(),
            std::fs::read(work.path().join("release-chart/Chart.yaml"))?,
        ),
        (
            format!("chart/{}", control::platform_build::BUILD_RECORD_FILE),
            b"{}".to_vec(),
        ),
        (
            "deploy/platform/release/Chart.yaml".into(),
            std::fs::read(repository.join("deploy/platform/release/Chart.yaml"))?,
        ),
        (
            "apps/platform_ui/dist/index.html".into(),
            b"<!doctype html><title>fixture</title>".to_vec(),
        ),
    ]
    .into();
    fixtures.applied.insert(
        "platform-control-credentials-test".into(),
        vec![
            wamn_control_provision::render_org_workload_secret_manifest(
                wamn_control_provision::WorkloadRoleFamily::Control,
                "test",
                "hosts",
                "postgresql://control:fixture@database/wamn_system",
            ),
            wamn_control_provision::render_control_administration_secret_manifest("test", "hosts"),
        ],
    );
    let mut operator = fixtures.control_operator.clone();
    operator["metadata"] = json!({"name":"operator-test","namespace":"hosts"});
    fixtures
        .applied
        .insert("platform-control-operator-test".into(), vec![operator]);
    std::fs::write(&fixtures.registry.registry_auth_file, "{}")?;
    for (_, environment) in &documents {
        use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleScope, WorkloadSecretBody};
        let mut manifest = fixture_release(
            environment
                .applications
                .values()
                .next()
                .context("fixture application")?
                .packages
                .iter()
                .next()
                .context("fixture package")?,
        )?;
        let component = manifest.components.first().context("fixture component")?;
        let member = wamn_catalog::ServingEntryMember {
            package_id: component.package_id.clone(),
            component: component.descriptor.component.clone(),
            digest: component.descriptor.component_digest.clone(),
        };
        manifest.composed_entry = Some(wamn_catalog::ServingComposedEntry {
            reference: format!("entries@sha256:{}", "1".repeat(64)),
            route_digest: format!("sha256:{}", "2".repeat(64)),
            application: member.clone(),
            members: [member].into(),
        });
        manifest =
            wamn_catalog::ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())?.0;
        let key = environment_key(environment);
        let cdc = wamn_control_provision::render_project_env_cdc_secret_manifest(
            &environment.triple(),
            "render01",
            "hosts",
            &format!("postgresql://fixture:fixture@database/wamn-db-{key}--render01"),
        );
        fixtures.cdc_credentials.insert(key.clone(), cdc.clone());
        fixtures
            .applied
            .insert(format!("platform-apply-{key}"), vec![cdc]);

        fixtures.releases.insert(key.clone(), manifest.clone());
        fixtures.web_clients.insert(key.clone(), BTreeMap::new());

        let database = wamn_control_provision::project_env_database_name(
            &environment.org,
            &environment.project,
            &environment.env,
            "render01",
        );
        fixtures.environment_urls.insert(
            key.clone(),
            format!("postgresql://fixture:fixture@database/{database}"),
        );
        let role = wamn_control_provision::workload_generation_role(
            WorkloadRoleFamily::SessionRoleReader,
            WorkloadRoleScope::ProjectEnvironment {
                org: &environment.org,
                project: &environment.project,
                environment: &environment.env,
                database: &database,
            },
            wamn_run_state::CredentialGeneration::A,
        )?;
        let target = wamn_control_provision::session_target::SessionTarget::new(
            &environment.triple(),
            "render01",
            "fixture",
            &format!("postgresql://{role}:fixture@database/{database}"),
        )?;
        fixtures.emitted.insert(
            format!("platform-session-{key}"),
            wamn_control_provision::render_workload_secret_manifest(
                WorkloadRoleFamily::SessionRoleReader,
                &environment.triple(),
                "identity",
                WorkloadSecretBody::SessionTarget(&target),
            ),
        );
    }
    let mut targets = vec![(
        cluster.route_hosts.control.clone(),
        "GET".to_owned(),
        "/api/wamn_control/control/mine".to_owned(),
    )];
    for (_, environment) in &documents {
        let (method, path) = ("GET".to_owned(), "/api/sample/read".to_owned());
        targets.push((environment.route_host.clone(), method, path));
        let key = environment_key(environment);
        let account = wamn_credential_store::kubernetes::service_account_name(
            &environment.org,
            &environment.project,
            &environment.env,
        );
        let secret_name = wamn_credential_store::kubernetes::secret_name(
            &environment.org,
            &environment.project,
            &environment.env,
        );
        let mut applied = control::environment::ensure::credential_access(&account, &secret_name)
            .into_iter()
            .map(|(_, mut object)| {
                object["metadata"]["namespace"] = json!("hosts");
                object
            })
            .collect::<Vec<_>>();
        applied.push(wamn_control_provision::render_project_env_database(
            &environment.triple(),
            "render01",
            "wamn-pg",
            "platform",
            None,
        ));
        applied.push(wamn_control_provision::render_project_env_secret_manifest(
            &environment.triple(),
            "hosts",
        ));
        let url = fixtures
            .environment_urls
            .get(&key)
            .context("environment URL fixture")?;
        for credential in wamn_control_provision::host_credentials::HOST_CREDENTIALS {
            let body = if credential.family == wamn_control_provision::WorkloadRoleFamily::App {
                wamn_control_provision::WorkloadSecretBody::EnvKeyUrl {
                    env_key: "fixture",
                    tenant_key: "fixture",
                    url,
                }
            } else {
                wamn_control_provision::WorkloadSecretBody::Url(url)
            };
            applied.push(wamn_control_provision::render_workload_secret_manifest(
                credential.family,
                &environment.triple(),
                "hosts",
                body,
            ));
        }
        fixtures
            .applied
            .insert(format!("platform-first-apply-{key}"), applied);
    }
    for (host, method, path) in targets {
        for (method, path, status) in std::iter::once((method.as_str(), path.as_str(), 401)).chain(
            fixtures
                .web_hosts
                .contains(&host)
                .then_some([("GET", "/", 200), ("GET", "/config.json", 200)])
                .into_iter()
                .flatten(),
        ) {
            let name = format!(
                "platform-https-{}",
                &control::platform::job::input_digest(&json!([host, method, path]))[..16]
            );
            fixtures.outputs.insert(
                name,
                json!({"http_code":status,"certs":CERTIFICATE}).to_string(),
            );
            fixtures.https_results.push(
                json!({"host":host,"method":method,"path":path,"status":status,"serial":"01"}),
            );
        }
    }
    let compiled = documents
        .iter()
        .map(|(_, document)| document.clone())
        .collect::<Vec<_>>();
    let objects = install(&cluster, &compiled, &repository, &fixtures)?;
    Ok(Render {
        _work: work,
        owner_inputs: vec![
            json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"identity-resend","namespace":"identity"},"stringData":{"api-key":"fixture-resend-key"}}),
        ],
        cluster,
        documents,
        fixtures,
        objects,
    })
}
