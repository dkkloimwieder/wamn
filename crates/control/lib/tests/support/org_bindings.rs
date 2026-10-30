//! Fixture setup through the org connection authority.
use anyhow::Context as _;
use serde_json::Value;
use wamn_control::environment::{
    self, Platform,
    document::{DeclaredRelease, EnvironmentDocument},
};

pub async fn bind(
    platform: &Platform,
    document: &mut EnvironmentDocument,
    application: &str,
    alias: &str,
    name: &str,
    integration: &str,
    mut parameters: Value,
) -> anyhow::Result<()> {
    let (mut system, driver) =
        tokio_postgres::connect(&platform.system_database_url, tokio_postgres::NoTls).await?;
    let driver = tokio::spawn(driver);
    system.batch_execute("SET ROLE wamn_system").await?;
    if system
        .query_opt(
            "SELECT env FROM registry.project_envs WHERE org=$1 AND project=$2 AND env=$3",
            &[&document.org, &document.project, &document.env],
        )
        .await?
        .is_none()
    {
        let mut empty = document.clone();
        empty.integrations.clear();
        empty.release = DeclaredRelease::None;
        for app in empty.applications.values_mut() {
            app.connections.clear();
        }
        let path = std::env::temp_dir().join(format!(
            "wamn-org-binding-{}-{}.k",
            std::process::id(),
            document.env
        ));
        std::fs::write(&path, empty.to_kcl())?;
        let result = Box::pin(environment::apply(platform, &path)).await;
        std::fs::remove_file(path)?;
        result?;
    }
    // The hosts of a cloud target read the environment Secret, and only a
    // local target reads credential files.
    let local: bool = system
        .query_one(
            "SELECT t.type='local' FROM registry.project_envs e JOIN registry.target t ON t.id=e.target_id \
             WHERE e.org=$1 AND e.project=$2 AND e.env=$3",
            &[&document.org, &document.project, &document.env],
        )
        .await?
        .get(0);
    let tx = system.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_control").await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    let (auth_type, credential) = if integration == "http" {
        if let Some(origin) = parameters["primary-authority"].as_str() {
            parameters["primary-authority"] = origin.trim_end_matches('/').into();
        }
        if name == super::http_connection::API_KEY_HANDLE {
            parameters["auth"] = serde_json::json!({"in":"query","name":"key"});
            ("api_key", serde_json::json!({"key":"fixture-api-key"}))
        } else if name == super::http_connection::AWS_HANDLE {
            parameters["auth"] = serde_json::json!({"region":"us-east-1","service":"execute-api"});
            (
                "aws_sigv4",
                serde_json::json!({"access_key_id":"fixture-access","secret_access_key":"fixture-private"}),
            )
        } else if name == super::http_connection::OUTBOUND_HANDLE {
            parameters["auth"] = serde_json::json!({"in":"header","name":"x-wamn-key"});
            (
                "api_key",
                serde_json::json!({"key":super::http_connection::OUTBOUND_SECRET}),
            )
        } else {
            (
                "http_basic",
                serde_json::json!({"username":"fixture","password":"fixture-password"}),
            )
        }
    } else if integration == "modbus" {
        ("none", serde_json::json!({}))
    } else {
        ("custom", serde_json::json!({}))
    };
    let referenced;
    let store: Box<dyn wamn_credential_store::Store + '_> = if local {
        Box::new(wamn_test_infrastructure::credentials::LocalCredentials::create()?)
    } else {
        referenced = cluster_credential(
            platform,
            &wamn_credential_store::kubernetes::secret_name(
                &document.org,
                &document.project,
                &document.env,
            ),
            credential.to_string().as_bytes(),
        )
        .await?;
        Box::new(wamn_test_infrastructure::credentials::ReferencedCredentials(&referenced))
    };
    wamn_test_infrastructure::credentials::define(&tx,&document.org,&serde_json::json!({"project":document.project,"env":document.env,"name":name,"integration_type":integration,"auth_type":auth_type,"external_system":if integration=="http" {Some(name)} else {None},"parameters":parameters,"credential":credential.to_string()}),store.as_ref()).await?;
    tx.commit().await?;
    document
        .applications
        .get_mut(application)
        .context("fixture application")?
        .connections
        .insert(alias.into(), name.into());
    document.integrations.insert(
        name.into(),
        environment::document::Integration { version: 1 },
    );
    driver.abort();
    Ok(())
}

/// Write `value` into the environment Secret of a cloud target, in the layout
/// of the k8s credential store: one key per credential version.
async fn cluster_credential(
    platform: &Platform,
    secret: &str,
    value: &[u8],
) -> anyhow::Result<wamn_credential_store::StoreVersion> {
    use base64::Engine as _;
    let target = &platform.target;
    let version = uuid::Uuid::new_v4().to_string();
    let data =
        serde_json::json!({ &version: base64::engine::general_purpose::STANDARD.encode(value) });
    let kubectl = |arguments: Vec<String>, input: Option<Vec<u8>>| async move {
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        wamn_control::kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            "hosts",
            &arguments,
            input.as_deref(),
        )
        .await
    };
    let existing = kubectl(
        ["get", "secret", secret, "--ignore-not-found", "-o", "name"]
            .map(str::to_owned)
            .into(),
        None,
    )
    .await?;
    if existing.trim().is_empty() {
        let created = serde_json::json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": secret, "namespace": "hosts"},
            "data": data,
        });
        kubectl(
            ["create", "-f", "-"].map(str::to_owned).into(),
            Some(created.to_string().into_bytes()),
        )
        .await?;
    } else {
        let patch = serde_json::json!({ "data": data }).to_string();
        kubectl(
            ["patch", "secret", secret, "--type", "merge", "-p", &patch]
                .map(str::to_owned)
                .into(),
            None,
        )
        .await?;
    }
    Ok(wamn_credential_store::StoreVersion {
        store: "k8s",
        reference: secret.to_owned(),
        version,
    })
}

/// Publish connection fixtures with one unused stateless operation each.
pub async fn publish_declarations(
    run: &mut super::Run,
    root: &std::path::Path,
    registry: &wamn_control::package_artifact::PackageRegistry,
    component_base: &str,
    database_url: &str,
) -> anyhow::Result<()> {
    use serde_json::json;
    use wamn_schema_generator::receipt::{RECEIPT_FILE, ReceiptComponent};
    for (name, connections) in [
        (
            "label_fixture",
            json!({"labels":{"interfaces":["wasmcloud:blobstore/blobstore@0.1.0"],"mode":"write"}}),
        ),
        (
            "http_fixture",
            json!({
                "upstream":{"interfaces":["wamn:connection/http@0.1.0"],"mode":"write"},
                "signed-upstream":{"interfaces":["wamn:connection/http@0.1.0"],"mode":"write"},
                "api-key-upstream":{"interfaces":["wamn:connection/http@0.1.0"],"mode":"write"},
                "aws-upstream":{"interfaces":["wamn:connection/http@0.1.0"],"mode":"write"}
            }),
        ),
    ] {
        let package = root.join(name);
        let declarations = package.join("publication/components");
        std::fs::create_dir_all(&declarations)?;
        let operation = format!("{}:fixture/run", name.replace('_', "-"));
        std::fs::write(
            declarations.join("fixture.json.in"),
            serde_json::to_vec(&json!({
                "scope":{"env_key":"__ENV_KEY__","package-id":name},
                "component":"fixture","interface-version":"0.1.0",
                "operations":{&operation:{"registered-operation":operation,
                    "input-ports":[],"output-ports":[],"parameters":[]}},
                "connections":[]
            }))?,
        )?;
        std::fs::write(
            package.join("wamn.k"),
            format!(
                r#"import manifest
manifest.Package {{
    package = {{id = "{name}", version = "1.0.0"}}
    required_platform_policy_contract = {{id = "fixture_data_access", state = "satisfied"}}
    models = {{}}
    components = {{fixture = {{connections = []}}}}
    connections = {connections}
    custom_operations = {{
        "fixture.run" = manifest.Command("fixture.run") {{
            connection = None
            transaction = None
            automatic_retry = None
            idempotent_by = "stateless"
            input.fields = []
            result = {{class = "one", fields = [{{path = "value", type = "text"}}]}}
            errors = ["invalid_input", "permission_denied", "internal_error"]
        }}
    }}
}}
"#
            ),
        )?;
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let palettes: &[(&str, &str)] = if name == "label_fixture" {
            &[("execution", "blob-put")]
        } else {
            &[
                ("no-std", "http-request"),
                ("no-std", "http-page"),
                ("no-std", "transform"),
            ]
        };
        let wirings = package.join("publication/wirings");
        std::fs::create_dir_all(&wirings)?;
        for (area, palette) in palettes {
            let target = root.join("platform").join(area).join(palette);
            std::fs::create_dir_all(&target)?;
            let template = std::fs::read_to_string(
                repository
                    .join("apps/platform")
                    .join(area)
                    .join(palette)
                    .join("declaration.json.in"),
            )?;
            let declaration = super::http_connection::declaration(&template)?;
            std::fs::write(
                target.join("declaration.json.in"),
                serde_json::to_vec(&declaration)?,
            )?;
        }
        let graphs = if name == "label_fixture" {
            vec![
                json!({"format-version":"0.1","wiring-id":"labels","version":1,"entry":"put","nodes":{"put":{"component":"blob-put","interface-version":"0.1.0","operation":"wamn:node/async-handler@0.1.0","params":{"store_alias":"labels","key_field":"/key","body_field":"/body"}}},"edges":[]}),
            ]
        } else {
            vec![
                serde_json::to_value(super::http_connection::wiring()?)?,
                serde_json::to_value(super::http_connection::paged_wiring()?)?,
            ]
        };
        for (index, graph) in graphs.into_iter().enumerate() {
            std::fs::write(
                wirings.join(format!("fixture-{index}.json")),
                serde_json::to_vec(&graph)?,
            )?;
        }
        let mut receipt = Box::pin(wamn_schema_generator::build::build_package(
            &wamn_schema_generator::build::BuildRequest::new(
                &package,
                &root.join("target/wamn"),
                database_url,
            ),
        ))
        .await?;
        // These packages live outside the app workspace, so build_package
        // generates their contracts but does not compile a guest. Encode an
        // unused guest with those exact exports for the normal admission path.
        let output = wamn_schema_generator::output_root(&package);
        let mut resolve = test_wit_parser::Resolve::new();
        resolve.push_str(
            "node.wit",
            include_str!("../../../../../crates/execution/workflow/router/wit/package.wit"),
        )?;
        let (wit_package, _) = resolve.push_dir(output.join("wit"))?;
        let world = resolve.select_world(&[wit_package], Some("generated"))?;
        let mut module = test_wit_component::dummy_module(
            &resolve,
            world,
            test_wit_parser::ManglingAndAbi::Standard32,
        );
        test_wit_component::embed_component_metadata(
            &mut module,
            &resolve,
            world,
            test_wit_component::StringEncoding::UTF8,
        )?;
        let bytes = test_wit_component::ComponentEncoder::default()
            .module(&module)?
            .validate(true)
            .encode()?;
        std::fs::create_dir_all(output.join("components"))?;
        std::fs::write(output.join("components/fixture.wasm"), &bytes)?;
        receipt.outputs.components = vec![ReceiptComponent {
            name: "fixture".into(),
            path: "components/fixture.wasm".into(),
            sha256: wamn_engine::component_admission::component_digest(&bytes),
        }];
        let target = std::path::PathBuf::from(
            std::env::var_os("CARGO_TARGET_DIR").context("CARGO_TARGET_DIR")?,
        );
        for (_, palette) in palettes {
            let path = format!("components/{palette}.wasm");
            if *palette == "blob-put" {
                let input = std::env::var_os("WAMN_BLOB_PUT_COMPONENT")
                    .context("WAMN_BLOB_PUT_COMPONENT")?;
                run.run(
                    &target
                        .join("debug/wamn-component-virtualizer")
                        .to_string_lossy(),
                    &[
                        "--input",
                        &std::path::PathBuf::from(input).to_string_lossy(),
                        "--output",
                        &output.join(&path).to_string_lossy(),
                    ],
                )
                .await?;
            } else {
                std::fs::copy(
                    target
                        .join("wasm32-wasip2/debug")
                        .join(format!("{}.wasm", palette.replace('-', "_"))),
                    output.join(&path),
                )?;
            }
            let bytes = std::fs::read(output.join(&path))?;
            receipt.outputs.components.push(ReceiptComponent {
                name: (*palette).into(),
                path,
                sha256: wamn_engine::component_admission::component_digest(&bytes),
            });
        }
        std::fs::write(output.join(RECEIPT_FILE), receipt.to_bytes())?;
        Box::pin(wamn_control::package_artifact::push_package(
            &wamn_control::package_artifact::PushPackageRequest {
                package,
                registry: registry.clone(),
                component_artifact_base: component_base.into(),
            },
        ))
        .await?;
    }
    Ok(())
}
