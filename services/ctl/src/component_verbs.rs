//! Arguments and output of the `bind-connection` verb.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, ValueEnum};
use wamn_catalog::RequirementType;
use wamn_control::bind_connection::{self, BindConnectionRequest};

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

    /// The host-held credential's handle. Never the credential. Omit it for a
    /// `gcs` definition, which signs with the host pod's service account.
    #[arg(long)]
    pub credential_handle: Option<String>,

    /// The release whose component is being bound, as its manifest digest.
    #[arg(long)]
    pub release_digest: String,

    /// The admitted component's digest, as the release manifest names it.
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
        definition: serde_json::from_slice(&std::fs::read(&args.definition).with_context(
            || {
                format!(
                    "read the generation definition {}",
                    args.definition.display()
                )
            },
        )?)
        .with_context(|| format!("{} is not JSON", args.definition.display()))?,
        credential_handle: args.credential_handle,
        manifest_digest: args.release_digest,
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
        request.manifest_digest,
        bound.definition_hash,
        bound.validation_hash
    );
    Ok(())
}
