//! Publish exact component bytes, then append their admitted catalog fact.
//!
//! This is the sole production caller of component byte admission. It validates
//! the local bytes before network I/O, publishes one digest-addressed OCI layer,
//! pulls the artifact back to verify its descriptor, config, and body, and only
//! then opens the control database to append `catalog.component_library`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use clap::Args;
use oci_client::client::{ClientConfig, ClientProtocol, Config, ImageLayer};
use oci_client::manifest::OciImageManifest;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use tokio_postgres::{Client as PgClient, Config as PgConfig, NoTls};
use wamn_catalog::{AdmittedComponent, ComponentDeclaration};
use wamn_runtime::component_admission::validate_component_admission;
use wamn_runtime::component_artifact::{
    component_artifact_config_bytes, component_artifact_layout, component_artifact_reference,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::registry_credentials::{RegistryCredentials, read_registry_credentials};

/// Bound each registry connect/read phase without adding a second deployment knob.
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

const INSERT_COMPONENT_SQL: &str = "INSERT INTO catalog.component_library (\
         tenant_id, catalog_id, catalog_version, component, interface_version, operation, \
         component_digest, imports, imports_fingerprint, input_ports, output_ports, parameters\
     ) VALUES (\
         $1, $2, $3, $4, $5, $6, $7, $8::text::jsonb, $9, \
         $10::text::jsonb, $11::text::jsonb, $12::text::jsonb\
     ) ON CONFLICT DO NOTHING RETURNING admitted_at";

const EXACT_COMPONENT_SQL: &str = "SELECT EXISTS (\
         SELECT 1 FROM catalog.component_library \
          WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
            AND component = $4 AND interface_version = $5 AND operation = $6 \
            AND component_digest = $7 AND imports = $8::text::jsonb \
            AND imports_fingerprint = $9 AND input_ports = $10::text::jsonb \
            AND output_ports = $11::text::jsonb AND parameters = $12::text::jsonb\
     )";

#[derive(Debug, Args)]
pub struct PushComponentArgs {
    /// Exact wasm component bytes to validate and publish.
    #[arg(long)]
    pub component_bytes: PathBuf,

    /// JSON declaration of catalog scope, component identity, operation, typed
    /// input/output ports, and parameters.
    #[arg(long)]
    pub declaration: PathBuf,

    /// Explicit `<registry>/<repository>` base. It must not include a tag or
    /// digest; the admitted component digest derives the immutable tag.
    #[arg(long)]
    pub artifact_base: String,

    /// Projected `.dockerconfigjson` file carrying the push credential.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,

    /// Use plain HTTP for exactly the registry host in `--artifact-base`.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,

    /// Exact admitted `wamn:<package>` capability. Repeat for each package the
    /// closed platform registry grants this component.
    #[arg(long = "admit-platform-package")]
    pub admitted_platform_packages: Vec<String>,

    /// Owner URL to the T1 control database carrying `catalog.component_library`.
    /// The connection is opened only after OCI publication verifies. Env
    /// `WAMN_SYSTEM_ADMIN_URL`.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,
}

/// Validate, publish, verify, and finally record one admitted component.
pub async fn run(args: PushComponentArgs) -> anyhow::Result<()> {
    let component_bytes = std::fs::read(&args.component_bytes)
        .with_context(|| format!("read component bytes {}", args.component_bytes.display()))?;
    let declaration_bytes = std::fs::read(&args.declaration)
        .with_context(|| format!("read component declaration {}", args.declaration.display()))?;
    let declaration: ComponentDeclaration = serde_json::from_slice(&declaration_bytes)
        .with_context(|| format!("parse component declaration {}", args.declaration.display()))?;

    let engine = wamn_runtime::build_engine(&[]).context("build component admission engine")?;
    let admitted = validate_component_admission(
        &engine,
        &component_bytes,
        wamn_runtime::component_admission::ComponentAdmissionRequest {
            declaration,
            admitted_platform_packages: args
                .admitted_platform_packages
                .into_iter()
                .collect::<BTreeSet<_>>(),
        },
    )
    .context("validate exact component bytes")?;
    let catalog_version = i32::try_from(admitted.scope.catalog_version)
        .context("catalog-version exceeds the PostgreSQL integer carrier")?;
    let database_config: PgConfig = args
        .system_database_url
        .parse()
        .context("parse T1 control database URL")?;
    let artifact = component_artifact_reference(&args.artifact_base, &admitted.component_digest)
        .context("derive component artifact reference")?;
    let reference = Reference::with_tag(
        artifact.registry().to_owned(),
        artifact.repository().to_owned(),
        artifact.tag().to_owned(),
    );
    let registry_credentials =
        read_registry_credentials(&args.registry_auth_file, artifact.registry())
            .context("load component registry push credential")?;
    let config_bytes = component_artifact_config_bytes(&admitted);

    publish_and_verify(
        &reference,
        &args.artifact_base,
        args.insecure_registry,
        &component_bytes,
        &config_bytes,
        &admitted,
        &registry_credentials,
    )
    .await?;

    persist_admitted_component(&database_config, &admitted, catalog_version).await?;
    println!("{}", admitted.component_digest);
    Ok(())
}

async fn publish_and_verify(
    reference: &Reference,
    artifact_base: &str,
    insecure: bool,
    component_bytes: &[u8],
    config_bytes: &[u8],
    component: &AdmittedComponent,
    credentials: &RegistryCredentials,
) -> anyhow::Result<()> {
    let protocol = if insecure {
        ClientProtocol::HttpsExcept(vec![reference.resolve_registry().to_owned()])
    } else {
        ClientProtocol::Https
    };
    let client = OciClient::new(ClientConfig {
        protocol,
        read_timeout: Some(REGISTRY_IO_TIMEOUT),
        connect_timeout: Some(REGISTRY_IO_TIMEOUT),
        ..ClientConfig::default()
    });
    let auth = RegistryAuth::Basic(
        credentials.username().to_owned(),
        credentials.password().to_owned(),
    );
    let (layer, config, manifest) = artifact_layout(component_bytes, config_bytes);

    client
        .push(
            reference,
            std::slice::from_ref(&layer),
            config,
            &auth,
            Some(manifest),
        )
        .await
        .with_context(|| format!("push component artifact {reference}"))?;

    // Publication is not a successful catalog admission until the immutable
    // reference can be read back by the production puller and independently
    // proves the descriptor, config facts, and exact component body.
    let source_config =
        ComponentArtifactSourceConfig::new(artifact_base, insecure, REGISTRY_IO_TIMEOUT)
            .context("configure published component verification source")?
            .with_credentials(credentials.clone());
    ComponentArtifactSource::new(source_config)
        .pull_verified(component)
        .await
        .with_context(|| format!("verify published component artifact {reference}"))?;
    Ok(())
}

fn artifact_layout(
    component_bytes: &[u8],
    config_bytes: &[u8],
) -> (ImageLayer, Config, OciImageManifest) {
    let layout = component_artifact_layout(component_bytes, config_bytes);
    let layer = ImageLayer::new(
        layout.component_bytes().to_vec(),
        layout.layer_media_type().to_owned(),
        None,
    );
    let config = Config::new(
        layout.config_bytes().to_vec(),
        layout.config_media_type().to_owned(),
        None,
    );
    let manifest = OciImageManifest::build(std::slice::from_ref(&layer), &config, None);
    (layer, config, manifest)
}

async fn persist_admitted_component(
    database_config: &PgConfig,
    component: &AdmittedComponent,
    catalog_version: i32,
) -> anyhow::Result<()> {
    let (mut client, connection) = database_config
        .connect(NoTls)
        .await
        .context("connect to T1 control database")?;
    let connection_task = tokio::spawn(connection);
    let stored = persist_with_client(&mut client, component, catalog_version).await;
    match stored {
        Ok(()) => {
            drop(client);
            connection_task
                .await
                .context("join T1 control database connection")?
                .context("drive T1 control database connection")
        }
        Err(error) => {
            connection_task.abort();
            Err(error)
        }
    }
}

async fn persist_with_client(
    client: &mut PgClient,
    component: &AdmittedComponent,
    catalog_version: i32,
) -> anyhow::Result<()> {
    let transaction = client
        .transaction()
        .await
        .context("begin component-library admission")?;
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&component.scope.tenant_id])
        .await
        .context("claim component-library tenant")?;
    append_or_verify_admitted_component(&transaction, component, catalog_version).await?;
    transaction
        .commit()
        .await
        .context("commit component-library admission")
}

/// Append one admitted component fact, or prove an exact retry.
///
/// The caller owns the transaction and tenant claim so release promotion can
/// combine this write with its target wiring and pointer cutover atomically.
pub(crate) async fn append_or_verify_admitted_component(
    transaction: &tokio_postgres::Transaction<'_>,
    component: &AdmittedComponent,
    catalog_version: i32,
) -> anyhow::Result<()> {
    let imports =
        serde_json::to_string(&component.imports).context("serialize admitted imports")?;
    let input_ports =
        serde_json::to_string(&component.input_ports).context("serialize admitted input ports")?;
    let output_ports = serde_json::to_string(&component.output_ports)
        .context("serialize admitted output ports")?;
    let parameters =
        serde_json::to_string(&component.parameters).context("serialize admitted parameters")?;
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 12] = [
        &component.scope.tenant_id,
        &component.scope.catalog_id,
        &catalog_version,
        &component.component,
        &component.interface_version,
        &component.operation,
        &component.component_digest,
        &imports,
        &component.imports_fingerprint,
        &input_ports,
        &output_ports,
        &parameters,
    ];

    let inserted = transaction
        .query_opt(INSERT_COMPONENT_SQL, &params)
        .await
        .context("append admitted component-library fact")?
        .is_some();
    if !inserted {
        let exact: bool = transaction
            .query_one(EXACT_COMPONENT_SQL, &params)
            .await
            .context("verify existing component-library fact")?
            .get(0);
        ensure!(exact, "component-library-fact-conflict");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use wamn_catalog::{ComponentCatalogScope, ComponentDeclaration};

    use super::*;

    #[test]
    fn persistence_is_append_only_and_exact_retry_only() {
        assert!(INSERT_COMPONENT_SQL.contains("ON CONFLICT DO NOTHING"));
        assert!(!INSERT_COMPONENT_SQL.contains("DO UPDATE"));
        assert!(EXACT_COMPONENT_SQL.contains("component_digest = $7"));
        assert!(EXACT_COMPONENT_SQL.contains("imports = $8::text::jsonb"));
        assert_eq!(
            CLAIM_TENANT_SQL,
            "SELECT set_config('app.tenant', $1, true)"
        );
    }

    #[test]
    fn production_publisher_layout_matches_the_puller_contract() {
        let component_bytes = b"component-bytes";
        let config_bytes = b"config-bytes";
        let expected = component_artifact_layout(component_bytes, config_bytes);
        let (layer, config, manifest) = artifact_layout(component_bytes, config_bytes);
        let digest = wamn_runtime::component_admission::component_digest(component_bytes);
        let reference = component_artifact_reference("registry.example/wamn/components", &digest)
            .expect("shared digest reference derives");

        assert_eq!(
            expected.layer_media_type(),
            "application/vnd.wamn.component.v1+wasm"
        );
        assert_eq!(
            expected.config_media_type(),
            "application/vnd.wamn.component.config.v1+json"
        );
        assert_eq!(expected.manifest_schema_version(), 2);
        assert_eq!(expected.layer_count(), 1);
        assert_eq!(reference.tag(), digest.trim_start_matches("sha256:"));
        assert_eq!(
            reference.to_string(),
            format!("registry.example/wamn/components:{}", reference.tag())
        );
        assert_eq!(&layer.data[..], expected.component_bytes());
        assert_eq!(layer.media_type, expected.layer_media_type());
        assert_eq!(&config.data[..], expected.config_bytes());
        assert_eq!(config.media_type, expected.config_media_type());
        assert_eq!(manifest.schema_version, expected.manifest_schema_version());
        assert_eq!(manifest.layers.len(), expected.layer_count());
        assert_eq!(manifest.layers[0].media_type, expected.layer_media_type());
        assert_eq!(
            manifest.layers[0].digest,
            wamn_runtime::component_admission::component_digest(component_bytes)
        );
        assert_eq!(
            manifest.layers[0].size,
            i64::try_from(component_bytes.len()).expect("fixture size fits")
        );
        assert_eq!(manifest.config.media_type, expected.config_media_type());
        assert_eq!(
            manifest.config.digest,
            wamn_runtime::component_admission::component_digest(config_bytes)
        );
        assert_eq!(
            manifest.config.size,
            i64::try_from(config_bytes.len()).expect("fixture size fits")
        );
    }

    #[tokio::test]
    #[ignore = "requires a disposable registry in WAMN_COMPONENT_ARTIFACT_BASE"]
    async fn production_publisher_and_puller_round_trip_exact_bytes() {
        let artifact_base = std::env::var("WAMN_COMPONENT_ARTIFACT_BASE")
            .expect("set WAMN_COMPONENT_ARTIFACT_BASE to a disposable HTTP registry/repository");
        let registry_auth_file = std::env::var("WAMN_REGISTRY_AUTH_FILE")
            .expect("set WAMN_REGISTRY_AUTH_FILE to its Docker config credential");
        let component_bytes = b"\0asm\r\0\x01\0";
        let engine = wamn_runtime::build_engine(&[]).expect("component admission engine builds");
        let component = validate_component_admission(
            &engine,
            component_bytes,
            wamn_runtime::component_admission::ComponentAdmissionRequest {
                declaration: ComponentDeclaration {
                    scope: ComponentCatalogScope {
                        tenant_id: "tenant-a".to_owned(),
                        catalog_id: "orders".to_owned(),
                        catalog_version: 1,
                    },
                    component: "round-trip".to_owned(),
                    interface_version: "0.1.0".to_owned(),
                    operation: "run".to_owned(),
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                },
                admitted_platform_packages: std::collections::BTreeSet::new(),
            },
        )
        .expect("fixture bytes admit");
        let artifact = component_artifact_reference(&artifact_base, &component.component_digest)
            .expect("fixture artifact reference derives");
        let reference = Reference::with_tag(
            artifact.registry().to_owned(),
            artifact.repository().to_owned(),
            artifact.tag().to_owned(),
        );
        let config_bytes = component_artifact_config_bytes(&component);
        let credentials = read_registry_credentials(
            PathBuf::from(registry_auth_file).as_path(),
            artifact.registry(),
        )
        .expect("load live registry credential");

        publish_and_verify(
            &reference,
            &artifact_base,
            true,
            component_bytes,
            &config_bytes,
            &component,
            &credentials,
        )
        .await
        .expect("production publisher and puller agree");
    }
}
