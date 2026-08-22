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
use oci_client::manifest::{OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use tokio_postgres::{Client as PgClient, Config as PgConfig, NoTls};
use wamn_catalog::{AdmittedComponent, ComponentDeclaration};
use wamn_runtime::component_admission::{component_digest, validate_component_admission};
use wamn_runtime::component_artifact::{
    COMPONENT_CONFIG_MEDIA_TYPE, COMPONENT_LAYER_MEDIA_TYPE, component_artifact_config_bytes,
    component_artifact_reference,
};

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
    let config_bytes = component_artifact_config_bytes(&admitted);

    publish_and_verify(
        &reference,
        artifact.registry(),
        args.insecure_registry,
        &component_bytes,
        &config_bytes,
        &admitted.component_digest,
    )
    .await?;

    persist_admitted_component(&database_config, &admitted, catalog_version).await?;
    println!("{}", admitted.component_digest);
    Ok(())
}

async fn publish_and_verify(
    reference: &Reference,
    registry: &str,
    insecure: bool,
    component_bytes: &[u8],
    config_bytes: &[u8],
    expected_digest: &str,
) -> anyhow::Result<()> {
    let protocol = if insecure {
        ClientProtocol::HttpsExcept(vec![registry.to_owned()])
    } else {
        ClientProtocol::Https
    };
    let client = OciClient::new(ClientConfig {
        protocol,
        read_timeout: Some(REGISTRY_IO_TIMEOUT),
        connect_timeout: Some(REGISTRY_IO_TIMEOUT),
        ..ClientConfig::default()
    });
    let auth = RegistryAuth::Anonymous;
    let layer = ImageLayer::new(
        component_bytes.to_vec(),
        COMPONENT_LAYER_MEDIA_TYPE.to_owned(),
        None,
    );
    ensure!(
        layer.sha256_digest() == expected_digest,
        "component-artifact-local-body-digest-mismatch"
    );
    let config = Config::new(
        config_bytes.to_vec(),
        COMPONENT_CONFIG_MEDIA_TYPE.to_owned(),
        None,
    );
    let config_digest = config.sha256_digest();
    let manifest = OciImageManifest::build(std::slice::from_ref(&layer), &config, None);
    verify_manifest(
        &manifest,
        expected_digest,
        component_bytes.len(),
        &config_digest,
        config_bytes.len(),
    )?;

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
    // reference can be read back and independently proves both bodies.
    let (published, _) = client
        .pull_image_manifest(reference, &auth)
        .await
        .with_context(|| format!("verify published component manifest {reference}"))?;
    let (published_layer, published_config) = verify_manifest(
        &published,
        expected_digest,
        component_bytes.len(),
        &config_digest,
        config_bytes.len(),
    )?;
    let mut returned_component = Vec::with_capacity(component_bytes.len());
    client
        .pull_blob(reference, published_layer, &mut returned_component)
        .await
        .with_context(|| format!("verify published component body {reference}"))?;
    ensure!(
        component_digest(&returned_component) == expected_digest,
        "component-artifact-published-body-digest-mismatch"
    );
    ensure!(
        returned_component == component_bytes,
        "component-artifact-published-body-bytes-mismatch"
    );
    let mut returned_config = Vec::with_capacity(config_bytes.len());
    client
        .pull_blob(reference, published_config, &mut returned_config)
        .await
        .with_context(|| format!("verify published component config {reference}"))?;
    ensure!(
        returned_config == config_bytes,
        "component-artifact-published-config-mismatch"
    );
    Ok(())
}

fn verify_manifest<'a>(
    manifest: &'a OciImageManifest,
    expected_component_digest: &str,
    expected_component_size: usize,
    expected_config_digest: &str,
    expected_config_size: usize,
) -> anyhow::Result<(&'a OciDescriptor, &'a OciDescriptor)> {
    ensure!(
        manifest.schema_version == 2,
        "component-artifact-manifest-schema-mismatch"
    );
    ensure!(
        manifest.layers.len() == 1,
        "component-artifact-layer-cardinality-mismatch"
    );
    let layer = &manifest.layers[0];
    ensure!(
        layer.media_type == COMPONENT_LAYER_MEDIA_TYPE,
        "component-artifact-layer-media-type-mismatch"
    );
    ensure!(
        layer.digest == expected_component_digest,
        "component-artifact-layer-digest-mismatch"
    );
    ensure!(
        layer.size == i64::try_from(expected_component_size).unwrap_or(i64::MAX),
        "component-artifact-layer-size-mismatch"
    );
    ensure!(
        manifest.config.media_type == COMPONENT_CONFIG_MEDIA_TYPE,
        "component-artifact-config-media-type-mismatch"
    );
    ensure!(
        manifest.config.digest == expected_config_digest,
        "component-artifact-config-digest-mismatch"
    );
    ensure!(
        manifest.config.size == i64::try_from(expected_config_size).unwrap_or(i64::MAX),
        "component-artifact-config-size-mismatch"
    );
    Ok((layer, &manifest.config))
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

    let transaction = client
        .transaction()
        .await
        .context("begin component-library admission")?;
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&component.scope.tenant_id])
        .await
        .context("claim component-library tenant")?;
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
    transaction
        .commit()
        .await
        .context("commit component-library admission")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(media_type: &str, digest: &str, size: i64) -> OciDescriptor {
        OciDescriptor {
            media_type: media_type.to_owned(),
            digest: digest.to_owned(),
            size,
            ..OciDescriptor::default()
        }
    }

    fn manifest(digest: &str) -> OciImageManifest {
        OciImageManifest {
            schema_version: 2,
            config: descriptor(COMPONENT_CONFIG_MEDIA_TYPE, "sha256:config", 17),
            layers: vec![descriptor(COMPONENT_LAYER_MEDIA_TYPE, digest, 23)],
            ..OciImageManifest::default()
        }
    }

    #[test]
    fn manifest_verifier_pins_both_descriptors_and_one_layer() {
        let digest = format!("sha256:{}", "a".repeat(64));
        verify_manifest(&manifest(&digest), &digest, 23, "sha256:config", 17)
            .expect("exact manifest verifies");

        let mut extra = manifest(&digest);
        extra.layers.push(extra.layers[0].clone());
        assert!(verify_manifest(&extra, &digest, 23, "sha256:config", 17).is_err());

        let mut wrong_media = manifest(&digest);
        wrong_media.layers[0].media_type = "application/wasm".to_owned();
        assert!(verify_manifest(&wrong_media, &digest, 23, "sha256:config", 17).is_err());

        let mut wrong_config = manifest(&digest);
        wrong_config.config.digest = "sha256:other".to_owned();
        assert!(verify_manifest(&wrong_config, &digest, 23, "sha256:config", 17).is_err());
    }

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
}
