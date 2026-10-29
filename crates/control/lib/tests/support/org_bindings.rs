//! Fixture setup through the org connection authority.
use anyhow::Context as _;
use serde_json::Value;
use wamn_control::environment::{
    self, Platform,
    document::{DeclaredRelease, EnvironmentDocument},
};
use wamn_platform_identity::{
    auth_changes::AuthDrops,
    connections::{self, ConnectionWrite},
};

pub async fn bind(
    platform: &Platform,
    document: &mut EnvironmentDocument,
    application: &str,
    alias: &str,
    name: &str,
    integration: &str,
    parameters: Value,
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
    let tx = system.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_control").await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    let old = tx.query_opt("SELECT c.id::text, s.name FROM registry.connection_rows($1) c LEFT JOIN registry.connection_system_rows($1) s ON s.id=c.external_system_id WHERE c.name=$2",&[&document.org,&name]).await?;
    let external_system = match old.as_ref().and_then(|row| row.get::<_, Option<String>>(1)) {
        Some(name) => Some(name),
        None if integration == "http" => Some(
            connections::write_external_system(
                &tx,
                &document.org,
                &connections::ExternalSystemWrite {
                    integration: integration.into(),
                    name: name.into(),
                    parameters: serde_json::json!({}),
                    status: "active".into(),
                },
            )
            .await?,
        ),
        None => None,
    };
    let mut drops = AuthDrops::default();
    let connection_name = connections::write_connection(
        &tx,
        &document.org,
        &ConnectionWrite {
            external_system,
            integration: integration.into(),
            name: name.into(),
            transport: "http".into(),
            exchange: "request".into(),
            parameters,
            status: "active".into(),
        },
        &mut drops,
    )
    .await?;
    connections::enable_connection(&tx, &document.org, &document.project, &connection_name).await?;
    for (scope, id) in [
        ("org", document.org.clone()),
        ("project", document.project.clone()),
        (
            "environment",
            format!("{}/{}", document.project, document.env),
        ),
    ] {
        connections::allow_integration(&tx, &document.org, scope, &id, integration).await?;
    }
    tx.commit().await?;
    platform.auth_changes().await?.publish(drops).await;
    document
        .applications
        .get_mut(application)
        .context("fixture application")?
        .connections
        .insert(alias.into(), name.into());
    document
        .applications
        .get_mut(application)
        .context("fixture application")?
        .connectors
        .insert(integration.into(), "host".into());
    driver.abort();
    Ok(())
}

/// Publish connection fixtures with one unused stateless operation each.
pub async fn publish_declarations(
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
            json!({"labels":{"integration":"s3","mode":"write"}}),
        ),
        (
            "http_fixture",
            json!({
                "upstream":{"integration":"http","mode":"write"},
                "signed-upstream":{"integration":"http","mode":"write"},
                "oauth2-upstream":{"integration":"http","mode":"write"},
                "oauth1-upstream":{"integration":"http","mode":"write"}
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
