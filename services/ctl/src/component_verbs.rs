//! Arguments and output of the `push-component` and `bind-connection` verbs.

use std::path::PathBuf;

use clap::{Args, ValueEnum};
use wamn_control::bind_connection::{self, BindConnectionRequest, RequirementType};
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
    let outcome = push_component::push_component(
        AdmitComponentRequest {
            package: args.package,
            component_bytes: args.component_bytes,
            declaration: args.declaration,
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
    .await?;
    print_published(&outcome);
    Ok(())
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
        "bound {}:{} to {} generation {} in release {} (definition {}, validation {})",
        request.component_digest,
        request.store_alias,
        bound.instance_id,
        bound.generation,
        request.effective_release_id,
        bound.definition_hash,
        bound.validation_hash
    );
    Ok(())
}
