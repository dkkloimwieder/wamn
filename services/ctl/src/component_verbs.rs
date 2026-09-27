//! Arguments and output of the `push-component` and `bind-connection` verbs.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Args, ValueEnum};
use serde_json::Value;
use wamn_control::bind_connection::{self, BindConnectionRequest, RequirementType};
use wamn_control::component_declaration::{authored_base_digests, render_declaration_document};
use wamn_control::push_component::{
    self, AdmitComponentRequest, PublishAdmittedComponentOutcome, PublishAdmittedComponentRequest,
};
use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;

#[derive(Debug, Args)]
pub struct PushComponentArgs {
    /// Package root whose strict wamn.json owns this component coordinate.
    #[arg(long)]
    pub package: PathBuf,

    /// Exact wasm component bytes to validate and publish.
    #[arg(long)]
    pub component_bytes: PathBuf,

    /// JSON declaration of catalog scope, component identity, operation, typed
    /// input/output ports, and parameters.
    #[arg(long, required_unless_present = "declaration_template")]
    pub declaration: Option<PathBuf>,

    /// Authored declaration template (`publication/components/*.json.in`).
    /// The verb renders it with `--tenant` and the base digests that the
    /// `wamn.json` of `--package` authors, as the dev coordinator does.
    #[arg(long, conflicts_with = "declaration", requires = "tenant")]
    pub declaration_template: Option<PathBuf>,

    /// Tenant that fills the `scope.tenant-id` slot of `--declaration-template`.
    #[arg(long, requires = "declaration_template")]
    pub tenant: Option<String>,

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

    /// PEM CA bundle trusted for the registry, on top of the compiled-in
    /// roots. Repeat or comma-delimit. Env `WASH_OCI_CA_PATHS`.
    #[arg(long = "oci-ca-path", env = OCI_CA_PATHS_ENV, value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,

    /// Exact admitted `wamn:<package>` capability. Repeat for each package the
    /// closed platform registry grants this component.
    #[arg(long = "admit-platform-package")]
    pub admitted_platform_packages: Vec<String>,

    /// Owner URL to the already-applied source project database. Env
    /// `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub project_database_url: String,

    /// Owner URL to the T1 control database. Env `WAMN_SYSTEM_ADMIN_URL`.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub control_database_url: String,
}

/// Validate, publish, verify, and record one component, then print the result.
pub async fn push(args: PushComponentArgs) -> anyhow::Result<()> {
    let rendered =
        match (&args.declaration_template, &args.tenant) {
            (Some(template), Some(tenant)) => Some(write_rendered_declaration(
                &render_declaration(&args.package, template, tenant)?,
            )?),
            _ => None,
        };
    let declaration = match (&rendered, args.declaration) {
        (Some(path), _) => path.clone(),
        (None, Some(path)) => path,
        (None, None) => {
            anyhow::bail!("push-component needs --declaration or --declaration-template")
        }
    };
    let result = push_component::push_component(
        AdmitComponentRequest {
            package: args.package,
            component_bytes: args.component_bytes,
            declaration,
            admitted_platform_packages: args.admitted_platform_packages,
        },
        PublishAdmittedComponentRequest {
            artifact_base: args.artifact_base,
            registry_auth_file: args.registry_auth_file,
            insecure_registry: args.insecure_registry,
            oci_ca_paths: args.oci_ca_paths,
            project_database_url: args.project_database_url,
            control_database_url: args.control_database_url,
        },
    )
    .await;
    if let Some(path) = &rendered {
        let _ = std::fs::remove_file(path);
    }
    print_published(&result?);
    Ok(())
}

/// Render an authored declaration template with the tenant and the base
/// digests that the package manifest authors.
fn render_declaration(package: &Path, template: &Path, tenant: &str) -> anyhow::Result<Value> {
    let base_digests = authored_base_digests(package).context("read the authored base digests")?;
    render_declaration_document(template, tenant, &base_digests)
        .context("render the component declaration")
}

/// Write the rendered declaration to a private file that the caller removes.
fn write_rendered_declaration(document: &Value) -> anyhow::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "wamn-ctl-declaration-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("read the clock")?
            .as_nanos()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(&serde_json::to_vec(document)?)
        .context("write the rendered declaration")?;
    Ok(path)
}

/// Print the lines that report one component publication.
pub fn print_published(outcome: &PublishAdmittedComponentOutcome) {
    println!(
        "projected {} (source-project: {}; control: {})",
        outcome.component_digest,
        if outcome.source_project_noop {
            "already converged"
        } else {
            "changed"
        },
        if outcome.control_noop {
            "already converged"
        } else {
            "changed"
        }
    );
    println!("{}", outcome.component_digest);
}

/// The one connection type the CLI can name today. The enum is the command
/// line's closed vocabulary, so a descriptor is never authored from a string.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum RequirementTypeArg {
    Blobstore,
}

impl RequirementTypeArg {
    fn requirement_type(self) -> RequirementType {
        match self {
            Self::Blobstore => RequirementType::Blobstore,
        }
    }
}

#[derive(Debug, Args)]
pub struct BindConnectionArgs {
    /// The project-environment database holding the catalog schema.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub database_url: String,

    #[arg(long)]
    pub tenant: String,

    #[arg(long)]
    pub environment: String,

    /// The environment-owned, stable identity of the connection instance.
    #[arg(long)]
    pub instance_id: String,

    /// Which platform descriptor the instance carries. Minted, never authored.
    #[arg(long, value_enum)]
    pub requirement_type: RequirementTypeArg,

    /// The generation's non-secret definition: a JSON object of exactly the
    /// coordinates the requirement type's plugin reads.
    #[arg(long, value_name = "PATH")]
    pub definition: PathBuf,

    /// The host-held credential's handle. Never the credential.
    #[arg(long)]
    pub credential_handle: String,

    /// The release whose component is being bound.
    #[arg(long)]
    pub effective_release_id: u32,

    /// The admitted component's digest, as push-component printed it.
    #[arg(long)]
    pub component_digest: String,

    /// The store alias that component declared for this connection.
    #[arg(long)]
    pub store_alias: String,
}

/// Bind one declared alias to an environment-owned instance and print the result.
pub async fn bind(args: BindConnectionArgs) -> anyhow::Result<()> {
    let request = BindConnectionRequest {
        database_url: args.database_url,
        tenant: args.tenant,
        environment: args.environment,
        instance_id: args.instance_id,
        requirement_type: args.requirement_type.requirement_type(),
        definition: args.definition,
        credential_handle: args.credential_handle,
        effective_release_id: args.effective_release_id,
        component_digest: args.component_digest,
        store_alias: args.store_alias,
    };
    let bound = bind_connection::bind(&request).await?;
    println!(
        "bound {}:{} to {} generation {} -> {} in release {} (definition {}, validation {}); configuration valid; connectivity and credentials not tested",
        request.component_digest,
        request.store_alias,
        bound.instance_id,
        bound
            .previous_generation
            .map_or_else(|| "none".to_owned(), |generation| generation.to_string()),
        bound.generation,
        request.effective_release_id,
        bound.definition_hash,
        bound.validation_hash
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_template_renders_as_the_dev_coordinator_renders() {
        let root = wamn_fixture_package::overlay_root();
        let template = root
            .join("publication/components")
            .join("fixture_overlay.json.in");
        let coordinator = render_declaration_document(
            &template,
            "dev",
            &authored_base_digests(&root).expect("read the fixture base digests"),
        )
        .expect("render as the dev coordinator does");
        let rendered = render_declaration(&root, &template, "dev").expect("render the template");
        assert_eq!(rendered, coordinator);
        assert_eq!(rendered["scope"]["tenant-id"], "dev");
    }
}
