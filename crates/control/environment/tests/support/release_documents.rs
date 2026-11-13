//! Canonical release documents shared by type and render-only cases.

use std::path::{Path, PathBuf};
use wamn_environment_control::environment::{
    self,
    document::{DeclaredRelease, EnvironmentDocument},
};
use wamn_release_control::publish_release::compose_application;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

pub const PACKAGE: &str = "edge_device";
/// The fixture release A: the fixture package, no http attachment, no
/// registration, a component no host pulls. `@VERSION@` is the package version.
pub const MANIFEST: &str = r#"{"attachments":{},"components":[{"descriptor":{"component":"http-request","component-digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{"input-ports":[],"output-ports":[],"parameters":[]}}},"package-id":"edge_device"}],"format-version":11,"release":{"packages":[{"package-id":"edge_device","package-version":"@VERSION@"}]},"routes":[]}"#;
/// The second fixture release, B: the same package, another component digest.
pub const MANIFEST_B: &str = r#"{"attachments":{},"components":[{"descriptor":{"component":"http-request","component-digest":"sha256:4444444444444444444444444444444444444444444444444444444444444444","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{"input-ports":[],"output-ports":[],"parameters":[]}}},"package-id":"edge_device"}],"format-version":11,"release":{"packages":[{"package-id":"edge_device","package-version":"@VERSION@"}]},"routes":[]}"#;
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
            .into_admitted(wamn_catalog::ComponentPackageCoordinate {
                project_env_key: String::new(),
                package_id: PACKAGE.to_owned(),
                package_version: version.to_owned(),
            })
            .component;
        value["components"][0] =
            serde_json::to_value(wamn_catalog::ServingComponent::project(&fact, Vec::new())?)?;
    }
    let bytes = wamn_digest::canonical_json_bytes(&value);
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

pub const NAMESPACE: &str = "hosts";
/// The one NATS of the test cluster.
pub const NATS_URL: &str = "nats://wamn-nats.platform.svc.cluster.local:4222";
pub const ORG: &str = "acme";
pub const PROJECT: &str = "orders";
/// The policy provision-org stamps for the trials template.
pub const POLICY: &str = "dev";
pub const PLATFORM_DOMAIN: &str = "example.invalid";

pub fn fixture_host_group(document: &EnvironmentDocument, local: &str) -> Value {
    let installed = wamn_environment_control::platform::control::host_values(&document.org);
    let shared =
        environment::ensure::materializer_host_values(document, 70, 1, 120, None, false, None)
            .expect("shared environment host");
    let mut group = serde_json::to_value(
        environment::runtime::host_group(
            &installed,
            Some(&shared),
            &format!("{local}/wamn/components"),
        )
        .expect("platform host inputs"),
    )
    .expect("host group");
    group["env"].as_array_mut().unwrap().push(json!({
        "name":"WAMN_ALLOWED_HOSTS", "value":http::ORIGIN
    }));
    group
}

pub fn fixture_document(env: &str, route_host: &str, digest: &str) -> EnvironmentDocument {
    EnvironmentDocument {
        schema: environment::document::Schema::default(),
        integrations: [(
            format!("labels-{env}"),
            environment::document::Integration { version: 1 },
        )]
        .into(),
        target: "cloud".into(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        env: env.to_owned(),
        policy: POLICY.to_owned(),
        revocation_seconds: 60,
        applications: std::collections::BTreeMap::from([(
            PACKAGE.to_owned(),
            environment::document::ApplicationSelection {
                current: DeclaredRelease::Digest(digest.to_owned()),
                previous: None,
                hostname: route_host.to_owned(),
                revocation_seconds: 5,
                client: None,
                connections: [("labels".to_owned(), format!("labels-{env}"))].into(),
                packages: std::collections::BTreeSet::from([
                    PACKAGE.to_owned(),
                    "label_fixture".to_owned(),
                ]),
            },
        )]),
    }
}

/// Compose the same pinned entry for the kind and local host paths.
pub fn fixture_http_entry(
    work: &Path,
    repository: &Path,
) -> anyhow::Result<(
    wamn_package_control::component_admission::ComponentAdmission,
    wamn_release_control::publish_release::ComposedApplication,
)> {
    let package = repository.join("apps").join(PACKAGE);
    let output = wamn_compiler::output_root(&package);
    let build_record = wamn_compiler::build_record::read(&output)?;
    let component = build_record
        .outputs
        .components
        .iter()
        .find(|component| component.name == "device")
        .context("the fixture build contains its device component")?;
    let declaration = wamn_package_control::component_declaration::render_package_declaration(
        &package,
        "device",
        &wamn_registry::project_env_key(ORG, PROJECT, "http"),
    )?;
    let declaration_path = work.join("device-declaration.json");
    std::fs::write(&declaration_path, serde_json::to_vec(&declaration)?)?;
    let admission = wamn_package_control::component_admission::admit_component(
        wamn_package_control::component_admission::AdmitComponentRequest {
            package,
            component_bytes: output.join(&component.path),
            declaration: declaration_path,
            admitted_platform_packages: vec!["wamn:node".to_owned(), "wamn:postgres".to_owned()],
            base_digests: std::collections::BTreeSet::new(),
        },
    )?;
    let route_path = std::env::var_os("WAMN_FLOW_HTTP_COMPONENT").map_or_else(
        || repository.join("apps/target/wasm32-wasip2/release/http_route.wasm"),
        PathBuf::from,
    );
    let route = std::fs::read(&route_path)
        .with_context(|| format!("read the pinned HTTP route guest {}", route_path.display()))?;
    let pin = format!(
        "sha256:{}",
        include_str!("../../../../../apps/platform/ingress/http-route/http_route.wasm.sha256")
            .split_whitespace()
            .next()
            .context("the HTTP route guest has a digest pin")?
    );
    let entry = compose_application(&admission, &route, &pin)?;
    Ok((admission, entry))
}

pub fn environment_releases() -> anyhow::Result<[Vec<u8>; 4]> {
    let [a, b, h] = fixture_releases()?;
    let (h, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&h)?;
    let device = &h
        .components
        .first()
        .context("the device component")?
        .descriptor;
    let components = [
        include_str!("../../../../../apps/platform/no-std/transform/declaration.json.in"),
        include_str!("../../../../../apps/platform/no-std/http-request/declaration.json.in"),
        include_str!("../../../../../apps/platform/no-std/http-page/declaration.json.in"),
    ]
    .into_iter()
    .map(|template| {
        let declaration = http::declaration(template).expect("a guest declaration");
        let mut descriptor = serde_json::to_value(device).expect("a descriptor");
        for field in [
            "component",
            "interface-version",
            "operations",
            "connections",
        ] {
            descriptor[field] = declaration[field].clone();
        }
        for operation in descriptor["operations"]
            .as_object_mut()
            .expect("guest operations")
            .values_mut()
        {
            for field in ["input-ports", "output-ports", "parameters"] {
                for item in operation[field].as_array_mut().expect("operation facts") {
                    item["schema"] = json!({"schema": item["schema"]});
                }
            }
        }
        serde_json::from_value(descriptor).expect("a fake guest descriptor")
    })
    .collect();
    let bytes = http::release(&h.canonical_bytes(), "2.1.0", components)
        .expect("the HTTP fixture release is canonical");
    let (http, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)
        .expect("the HTTP fixture release");
    Ok([a, b, h.canonical_bytes(), http.canonical_bytes()])
}

pub mod http {
    use super::{NAMESPACE, ORG, PACKAGE, PROJECT};
    use anyhow::{Context as _, ensure};
    use serde_json::{Value, json};
    use std::collections::BTreeSet;
    use std::path::Path;
    use wamn_catalog::{
        ComponentDescriptor, ComponentPackageCoordinate, ServingComponent, ServingManifest,
        WiringDocument,
    };
    use wamn_environment_control::environment::document::EnvironmentDocument;

    pub const SERVICE: &str = "http-fixture";
    pub const ORIGIN: &str = "http://http-fixture:8080";
    pub const INBOUND_HANDLE: &str = "inbound-signature";
    pub const INBOUND_SECRET: &str = "fixture-inbound-key";
    pub const OUTBOUND_HANDLE: &str = "signed-upstream";
    pub const OUTBOUND_SECRET: &str = "fixture-outbound-key";
    pub const SCHEDULE: &str = "http-minute";
    pub const API_KEY_HANDLE: &str = "api-key-upstream";
    pub const AWS_HANDLE: &str = "aws-upstream";

    /// Admit the built bytes with the declarations used by the kind fixture.
    pub fn admit_guest(
        repository: &Path,
        version: &str,
        package: &str,
        name: &str,
        bytes: &[u8],
    ) -> anyhow::Result<wamn_catalog::AdmittedComponentFacts> {
        let template = std::fs::read_to_string(
            repository.join(format!("apps/platform/no-std/{name}/declaration.json.in")),
        )?;
        let declaration = serde_json::from_value(declaration(
            &template
                .replace(
                    "__PROJECT_ENV_KEY__",
                    &wamn_registry::project_env_key(ORG, PROJECT, "http"),
                )
                .replace("__PACKAGE_ID__", package)
                .replace("__PACKAGE_VERSION__", version),
        )?)?;
        let engine = wamn_engine::build_engine(&[])?;
        let admitted = wamn_engine::component_admission::validate_component_admission(
            &engine,
            bytes,
            wamn_engine::component_admission::ComponentAdmissionRequest {
                declaration,
                admitted_platform_packages: ["wamn:node".to_owned(), "wamn:connection".to_owned()]
                    .into(),
                base_digests: BTreeSet::new(),
            },
        )?;
        Ok(admitted)
    }

    /// Declare the outbound handles before admission for ordinary binding.
    pub fn declaration(template: &str) -> anyhow::Result<Value> {
        let mut declaration: Value = serde_json::from_str(template)?;
        if declaration["component"] == "http-request" {
            for handle in [OUTBOUND_HANDLE, API_KEY_HANDLE, AWS_HANDLE] {
                declaration["connections"]
                    .as_array_mut()
                    .expect("HTTP requirements")
                    .push(json!({"store-alias": handle, "requirement-type": "http"}));
            }
        }
        Ok(declaration)
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
                    "params": {"requirement": "upstream", "method": "POST", "path-and-query": "/reuse"}}
            },
            "edges": [{"from": "input", "to": "send"}]
        }))?)
    }

    pub fn signed_wiring() -> anyhow::Result<WiringDocument> {
        let mut graph = serde_json::to_value(wiring()?)?;
        graph["wiring-id"] = json!("http-signed-send");
        graph["nodes"]["send"]["params"]["requirement"] = json!(OUTBOUND_HANDLE);
        graph["nodes"]["send"]["params"]["path-and-query"] = json!("/signed");
        Ok(serde_json::from_value(graph)?)
    }

    pub fn authentication_wirings() -> anyhow::Result<Vec<WiringDocument>> {
        [
            ("http-api-key", API_KEY_HANDLE, "/api-key"),
            ("http-aws", AWS_HANDLE, "/aws?z=a%2Bb&x=hello%20world&x=two"),
        ]
        .into_iter()
        .map(|(id, handle, path)| {
            let mut graph = serde_json::to_value(wiring()?)?;
            graph["wiring-id"] = json!(id);
            graph["nodes"]["send"]["params"] = json!({
                "requirement":handle, "method":"GET", "path-and-query":path
            });
            Ok(serde_json::from_value(graph)?)
        })
        .collect()
    }

    pub fn paged_wiring() -> anyhow::Result<WiringDocument> {
        let mut graph = serde_json::to_value(wiring()?)?;
        graph["wiring-id"] = json!("http-pages");
        graph["nodes"]["send"]["component"] = json!("http-page");
        graph["nodes"]["send"]["params"] = json!({
            "requirement": "upstream", "path-and-query": "/pages?limit=2",
            "page": {"style": "offset", "limit": "limit", "offset": "offset"},
            "items": "/items"
        });
        Ok(serde_json::from_value(graph)?)
    }

    pub fn scheduled_wiring() -> anyhow::Result<WiringDocument> {
        let mut graph = serde_json::to_value(wiring()?)?;
        graph["wiring-id"] = json!(SCHEDULE);
        graph["nodes"]["send"]["params"]["path-and-query"] = json!("/scheduled");
        Ok(serde_json::from_value(graph)?)
    }

    pub fn release(
        h: &[u8],
        _version: &str,
        components: Vec<ComponentDescriptor>,
    ) -> anyhow::Result<Vec<u8>> {
        let target = ComponentPackageCoordinate {
            project_env_key: wamn_registry::project_env_key(ORG, PROJECT, "http"),
            package_id: "http_fixture".to_owned(),
            package_version: "1.0.0".to_owned(),
        };
        let facts: Vec<_> = components
            .iter()
            .cloned()
            .map(|descriptor| descriptor.into_admitted(target.clone()).component)
            .collect();
        let (mut manifest, _) = ServingManifest::from_canonical_bytes(h)?;
        manifest
            .components
            .retain(|component| component.package_id != "label_fixture");
        manifest
            .release
            .packages
            .retain(|package| package.package_id() != "label_fixture");
        manifest
            .release
            .packages
            .insert(wamn_catalog::PackageCoordinate::new(
                "http_fixture",
                "1.0.0",
            )?);
        for (descriptor, fact) in components.into_iter().zip(&facts) {
            manifest
                .components
                .insert(ServingComponent::project(fact, descriptor.connections)?);
        }
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
        let records = route_requests(log)?;
        let requests: Vec<_> = records
            .iter()
            .map(|record| record["head"].as_str().unwrap_or_default())
            .collect();
        ensure!(
            requests.len() == 1,
            "fixture saw {} requests",
            requests.len()
        );
        ensure!(
            requests[0].lines().next() == Some("POST /reuse HTTP/1.1"),
            "fixture received the wrong target or protocol"
        );
        ensure!(
            records[0]["body"] == json!(br#"{"id":1}"#.as_slice()),
            "fixture received the wrong body"
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
            authorization == ["Basic Zml4dHVyZTpmaXh0dXJlLXBhc3N3b3Jk"],
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

    pub fn request_records(log: &str) -> anyhow::Result<Vec<Value>> {
        log.lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }

    fn route_requests(log: &str) -> anyhow::Result<Vec<Value>> {
        Ok(request_records(log)?
            .into_iter()
            .filter(|record| {
                // The schedule has its own target, so its traffic cannot count as a route request.
                record["head"].as_str().is_some_and(|head| {
                    head.starts_with("POST /reuse HTTP/1.1\r\n")
                        || head.starts_with("POST /signed HTTP/1.1\r\n")
                })
            })
            .collect())
    }

    pub fn credential_secret(
        environment: &str,
        values: &Value,
        references: &mut serde_json::Map<String, Value>,
    ) -> anyhow::Result<Value> {
        let name = wamn_registry::credential_secret_name(ORG, PROJECT, environment);
        let mut data = serde_json::Map::new();
        for reference in references.values_mut() {
            let version = reference["version"].as_str().context("fixture version")?;
            let material = values["items"]
                .as_array()
                .context("fixture resources")?
                .iter()
                .find_map(|item| item["stringData"].get(version))
                .context("fixture credential reference")?;
            data.insert(version.to_owned(), material.clone());
            // The host's Role permits only its own environment's Secret.
            reference["reference"] = name.clone().into();
        }
        Ok(json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": name, "namespace": NAMESPACE},
            "stringData": data,
        }))
    }
    pub fn bindings_match(rows: &Value, document: &EnvironmentDocument, digest: &str) -> bool {
        rows["versions"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(|row| row["version"] == 1))
            && ["upstream", OUTBOUND_HANDLE, API_KEY_HANDLE, AWS_HANDLE]
                .iter()
                .all(|handle| {
                    let bindings: Vec<_> = rows["bindings"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|row| row["name"] == *handle)
                        .collect();
                    bindings.len() == if *handle == "upstream" { 2 } else { 1 }
                        && bindings.iter().all(|row| {
                            row["manifest_digest"] == digest
                                && row["package_id"] == "http_fixture"
                                && row["mode"] == "write"
                                && row["integration_name"]
                                    == document.applications[PACKAGE].connections[*handle]
                        })
                })
    }
}

pub mod workflow {
    use super::{ORG, PLATFORM_DOMAIN, POLICY, PROJECT};
    use anyhow::Context as _;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::path::Path;
    use wamn_environment_control::environment::document::{DeclaredRelease, EnvironmentDocument};
    const ENVIRONMENT: &str = "integration";
    const CONSUMER: &str = "rest_consumer_fixture";
    use wamn_broker::Publisher;
    use wamn_catalog::WiringDocument;
    use wamn_environment_control::author_workflow::{
        AuthorWorkflowRequest, WorkflowExecution, WorkflowTrigger, author_workflow_in_databases,
    };
    use wamn_environment_control::environment;
    use wamn_environment_control::workflow_lifecycle::{
        WorkflowVersion, disable_workflow, enable_workflow,
    };

    /// The user who authors and enables schedule workflows, as an environment
    /// admin does. It holds each execution role it assigns.
    pub const ENABLER: &str = "00000000-0000-4000-8000-0000000000e1";

    /// One environment in which a test authors workflows.
    #[derive(Debug)]
    pub struct WorkflowEnvironment<'a> {
        pub system_url: &'a str,
        pub project_url: &'a str,
        pub target: &'a wamn_registry::Triple,
        pub project_env_key: &'a str,
        /// The release the environment serves.
        pub digest: &'a str,
        /// The applications the environment declares, as the serving Helm
        /// revision keeps them.
        pub applications: &'a wamn_catalog::EnvironmentApplications,
        pub auth: &'a Publisher,
    }

    impl WorkflowEnvironment<'_> {
        fn version<'a>(
            &'a self,
            application: &'a str,
            workflow_id: &'a str,
        ) -> WorkflowVersion<'a> {
            WorkflowVersion {
                org: &self.target.org,
                project: &self.target.project,
                environment: &self.target.env,
                application,
                workflow_id,
                version: 1,
            }
        }
    }

    pub async fn connect_owned(
        url: &str,
    ) -> anyhow::Result<(tokio_postgres::Client, tokio::task::JoinHandle<()>)> {
        let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls).await?;
        let task = tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok((client, task))
    }

    /// Admit a manual fixture run through the application host route.
    pub async fn start_workflow(
        work: &Path,
        environment: &WorkflowEnvironment<'_>,
        application: &str,
        workflow_id: &str,
        input: &Value,
        key: &str,
    ) -> anyhow::Result<String> {
        let (project, task) = connect_owned(environment.project_url).await?;
        let result = wamn_test_postgres::run_result::require_success(
            work,
            "start_workflow",
            &project,
            start_workflow_inner(&project, environment, application, workflow_id, input, key),
        )
        .await;
        task.abort();
        result
    }

    async fn start_workflow_inner(
        project: &tokio_postgres::Client,
        environment: &WorkflowEnvironment<'_>,
        application: &str,
        workflow_id: &str,
        input: &Value,
        key: &str,
    ) -> anyhow::Result<String> {
        use std::sync::Arc;
        use wamn_engine::router_delivery::{
            DeliveryOutcome, DeliveryRequest, RouteDelivery as _, Source,
        };
        use wamn_runtime::plugins::wamn_postgres::{
            AuthorityClass, ClassCredentials, WamnPostgres, WamnPostgresConfig,
        };

        project
            .execute(
                "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:schedule-fixture', false)",
                &[&wamn_identity_protocol::PlatformComponent::Provisioning
                    .principal_id()
                    .to_string()],
            )
            .await?;
        project.execute(
        "INSERT INTO platform.user_role (user_id, role_name) VALUES ($1::text::uuid, 'admin') ON CONFLICT DO NOTHING",
        &[&ENABLER],
    ).await?;
        let bytes: Vec<u8> = project
        .query_one(
            "SELECT canonical_bytes FROM catalog.releases WHERE project_env_key=$1 AND manifest_digest=$2",
            &[&environment.project_env_key, &environment.digest],
        )
        .await?
        .get(0);
        let release = Arc::new(
            wamn_engine::release_manifest::LoadedRelease::load_canonical_bytes(
                &bytes,
                "manual fixture workflow",
                wamn_engine::release_manifest::ReleaseTarget::new(
                    environment.project_env_key,
                    environment.target.env.as_str(),
                )
                .with_applications(environment.applications.clone()),
            )?,
        );
        let postgres = Arc::new(WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(
                ClassCredentials::default()
                    .with_class(
                        AuthorityClass::ExecutorPlatform,
                        environment.project_url.to_owned(),
                    )
                    .with_class(
                        AuthorityClass::Administration,
                        environment.project_url.to_owned(),
                    )
                    .with_class(
                        AuthorityClass::CallableHttp,
                        environment.project_url.to_owned(),
                    ),
            ),
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 5_000,
            statement_timeout_ms: 10_000,
            row_limit: 10_000,
        })?);
        let held = postgres
            .session_operation_permissions(
                "default",
                environment.project_env_key,
                &["admin".parse().unwrap()],
                &ENABLER.parse()?,
            )
            .await?;
        let attachment = wamn_catalog::HostRouteSet::Application
            .attachments()
            .find(|(_, attachment)| attachment.reference == "wamn-control:workflow/start")
            .map(|(id, _)| id.to_owned())
            .context("workflow.start attachment")?;
        let caller = wamn_engine::flow_http_routing::AuthenticatedCaller::new(
            attachment.clone(),
            ENABLER,
            wamn_engine::flow_http_routing::CredentialType::Session,
            held.admin,
            held.references.iter().cloned().collect(),
            held.active_role(),
        );
        let host = wamn_execution_host::HostRouteDelivery::new(
            release,
            wamn_execution_host::HostRouteHandlers::Application {
                postgres,
                project: "default".to_owned(),
                identity: None,
                changes: wamn_broker::Publisher::default(),
            },
            None,
        );
        let report = host.deliver(DeliveryRequest {
        source: Source::Attachment(attachment), delivery_id: key.to_owned(),
        payload: json!([{"request_id":key, "value":{
            "application":application,"workflow_id":workflow_id,"input_json":input,"idempotency_key":key
        }}]).to_string(),
        caller: None, trace: None, parent_causation: None, if_none_match: None,
    }, Some(caller)).await;
        let Ok(DeliveryOutcome::Respond(body)) = report.outcome else {
            anyhow::bail!("workflow.start failed: {:?}", report.outcome);
        };
        let body: Value = serde_json::from_str(&body)
            .with_context(|| format!("workflow.start response: {body}"))?;
        body[0]["value"]["run_id"]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("workflow.start refused: {body}"))
    }

    /// Admit a manual fixture run as `workflow.start` admits it, for a release
    /// that serves no application host routes. The HTTP environment has no
    /// session issuer, so its release cannot serve them. The run records the
    /// served release, so the deployed executor leases it.
    pub async fn admit_workflow(
        work: &Path,
        environment: &WorkflowEnvironment<'_>,
        application: &str,
        workflow_id: &str,
        input: &Value,
        key: &str,
    ) -> anyhow::Result<String> {
        let (project, task) = connect_owned(environment.project_url).await?;
        let result = wamn_test_postgres::run_result::require_success(
            work,
            "admit_workflow",
            &project,
            admit_workflow_inner(&project, environment, application, workflow_id, input, key),
        )
        .await;
        task.abort();
        result
    }

    async fn admit_workflow_inner(
        project: &tokio_postgres::Client,
        environment: &WorkflowEnvironment<'_>,
        application: &str,
        workflow_id: &str,
        input: &Value,
        key: &str,
    ) -> anyhow::Result<String> {
        use wamn_runtime::plugins::wamn_postgres::{
            AuthorityClass, ClassCredentials, ManualRunAdmission, ManualRunAdmitted, WamnPostgres,
            WamnPostgresConfig,
        };

        let bytes: Vec<u8> = project
        .query_one(
            "SELECT canonical_bytes FROM catalog.releases WHERE project_env_key=$1 AND manifest_digest=$2",
            &[&environment.project_env_key, &environment.digest],
        )
        .await?
        .get(0);
        let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)?;
        let postgres = WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::default().with_class(
                AuthorityClass::ExecutorPlatform,
                environment.project_url.to_owned(),
            )),
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 5_000,
            statement_timeout_ms: 10_000,
            row_limit: 10_000,
        })?;
        let workflow = postgres
            .enabled_workflow(
                "default",
                environment.project_env_key,
                environment.target.env.as_str(),
                application,
                workflow_id,
            )
            .await?
            .context("the workflow is enabled")?;
        let role = workflow
            .execution_role
            .as_deref()
            .context("the fixture workflow runs with a role")?;
        let packages = &environment
            .applications
            .get(application)
            .context("the environment declares the application")?
            .packages;
        let node_digests: serde_json::Map<String, Value> =
            wamn_catalog::resolve_workflow_components(
                environment.project_env_key,
                packages,
                &manifest,
                &workflow.graph,
            )
            .map_err(|error| anyhow::anyhow!("{error:?}"))?
            .into_iter()
            .map(|(node, component)| (node, Value::String(component.component_digest)))
            .collect();
        let admitted = postgres
            .admit_manual_run(
                "default",
                environment.project_env_key,
                &ManualRunAdmission {
                    package_id: application,
                    manifest_digest: environment.digest,
                    environment: environment.target.env.as_str(),
                    workflow_id,
                    workflow_version: workflow.version,
                    wiring_hash: &workflow.wiring_hash,
                    execution_principal_id: &wamn_identity_protocol::workflow_user_id().to_string(),
                    execution_role: role,
                    invoked_by_user_id: ENABLER,
                    node_digests: &Value::Object(node_digests),
                    idempotency_key: &format!("manual:{ENABLER}:{workflow_id}:{key}"),
                    input,
                },
            )
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let ManualRunAdmitted::Queued { run_id } = admitted else {
            anyhow::bail!("the fixture key {key} admitted no new run: {admitted:?}");
        };
        Ok(run_id)
    }

    /// The first minute boundary strictly after `after`.
    pub fn next_minute(after: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(after.timestamp().div_euclid(60) * 60 + 60, 0)
            .expect("a minute boundary after a valid time is a valid time")
    }

    /// Author `document` as a minutely schedule workflow of `application` and
    /// enable it, as an environment admin does with
    /// `wamn-ctl workflow author --schedule '* * * * *'` and `enable`. The admin
    /// first creates `role` with exactly `operations` and grants it to the
    /// `workflow` user and to itself, so the runs execute as the `workflow` user
    /// with `role`. A passing test report under the graph's hash stands in for the
    /// test.
    pub async fn enable_schedule(
        environment: &WorkflowEnvironment<'_>,
        application: &str,
        document: &WiringDocument,
        role: &str,
        operations: &[&str],
    ) -> anyhow::Result<()> {
        enable_trigger(
            environment,
            application,
            document,
            role,
            operations,
            WorkflowTrigger::Schedule("* * * * *"),
            None,
        )
        .await
    }

    pub async fn enable_trigger(
        environment: &WorkflowEnvironment<'_>,
        application: &str,
        document: &WiringDocument,
        role: &str,
        operations: &[&str],
        trigger: WorkflowTrigger<'_>,
        events: Option<&wamn_broker::EventBroker>,
    ) -> anyhow::Result<()> {
        let project_env_key = environment.project_env_key;
        let (mut project, project_task) = connect_owned(environment.project_url).await?;
        project
            .execute(
                "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:schedule-fixture', false)",
                &[&wamn_identity_protocol::PlatformComponent::Provisioning
                    .principal_id()
                    .to_string()],
            )
            .await?;
        project
            .execute(
                "INSERT INTO platform.\"user\" (id, type, email) \
             VALUES ($1::text::uuid, 'user', $2) ON CONFLICT (id) DO NOTHING",
                &[&ENABLER, &format!("schedule-enabler@{PLATFORM_DOMAIN}")],
            )
            .await?;
        project
            .execute("INSERT INTO platform.role (name) VALUES ($1)", &[&role])
            .await?;
        for operation in operations {
            project
                .execute(
                    "INSERT INTO platform.permission (role_name, permission) \
                 VALUES ($1, $2)",
                    &[&role, operation],
                )
                .await?;
        }
        for user in [
            format!("schedule-enabler@{PLATFORM_DOMAIN}"),
            format!("workflow@{PLATFORM_DOMAIN}"),
        ] {
            async {
                let tx = project.transaction().await?;
                let user_id: String = tx
                    .query_one(
                        "SELECT id::text FROM platform.\"user\" WHERE email=$1",
                        &[&user],
                    )
                    .await?
                    .get(0);
                let mut drops = wamn_broker::auth_changes::AuthDrops::default();
                let outcome = wamn_platform_identity::application::mutate_grant(
                    &tx,
                    project_env_key,
                    wamn_platform_identity::application::GrantAuthority::Provisioning,
                    wamn_platform_identity::application::GrantChange::UserRole {
                        user_id: &user_id,
                        role,
                        grant: true,
                    },
                    &mut drops,
                )
                .await?;
                tx.commit().await?;
                environment.auth.publish(drops).await;
                anyhow::Ok(outcome)
            }
            .await?;
        }
        author_workflow_in_databases(
            environment.project_url,
            &AuthorWorkflowRequest {
                org: &environment.target.org,
                project: &environment.target.project,
                environment: &environment.target.env,
                application,
                declared: environment.applications,
                document,
                trigger,
                execution: WorkflowExecution::Role(role),
                created_by: ENABLER,
                manifest_digest: environment.digest,
            },
        )
        .await?;
        enable_workflow(
            environment.system_url,
            environment.target,
            &mut project,
            &environment.version(application, &document.wiring_id),
            ENABLER,
            environment.digest,
            environment.applications,
            events,
        )
        .await?;
        project_task.abort();
        Ok(())
    }

    /// Disable the schedule workflow `workflow_id` of `application`, as an
    /// environment admin does with `wamn-ctl workflow disable`.
    pub async fn disable_schedule(
        environment: &WorkflowEnvironment<'_>,
        application: &str,
        workflow_id: &str,
    ) -> anyhow::Result<()> {
        let (mut project, project_task) = connect_owned(environment.project_url).await?;
        disable_workflow(
            environment.system_url,
            environment.target,
            &mut project,
            &environment.version(application, workflow_id),
        )
        .await?;
        project_task.abort();
        Ok(())
    }

    pub fn fixture_document(connector: &str) -> EnvironmentDocument {
        EnvironmentDocument {
            schema: environment::document::Schema::default(),
            integrations: std::collections::BTreeMap::new(),
            target: "cloud".into(),
            org: ORG.into(),
            project: PROJECT.into(),
            env: ENVIRONMENT.into(),
            policy: POLICY.into(),
            revocation_seconds: 60,
            // The existing consumer and integration packages put this application's
            // models in two schemas. The connector supplies the workflow components.
            applications: BTreeMap::from([(
                connector.to_owned(),
                wamn_environment_control::environment::document::ApplicationSelection {
                    current: DeclaredRelease::None,
                    previous: None,
                    hostname: "integration.orders.example".into(),
                    revocation_seconds: 5,
                    client: None,
                    connections: std::collections::BTreeMap::new(),
                    packages: [connector, "wamn_integration", CONSUMER]
                        .map(str::to_owned)
                        .into(),
                },
            )]),
        }
    }
}
