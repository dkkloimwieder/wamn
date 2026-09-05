//! `wamn-ctl bind-connection`: the connection-admin verb.
//!
//! Writes, in one transaction on the project-environment database, the three
//! rows a bound connection is made of -- an environment-owned INSTANCE, its
//! first immutable GENERATION, and the release-scoped BINDING of one admitted
//! component's declared store alias to that instance. The host holds the
//! credential; this verb stores only a handle to it.
//!
//! THIN BY RULE (wamn-362o.33). Arguments in; the control library's builders
//! in `wamn_schema_control::connections` are the sole SQL truth; the
//! runtime's `connection_generation::definition_hash` is the sole hasher. No
//! second wire contract, no second hasher.
//!
//! A DESCRIPTOR/DEFINITION MISMATCH IS UNCONSTRUCTIBLE HERE. The descriptor is
//! minted from the platform's own constructor for the requirement type given
//! (never authored), and the definition is checked against the coordinates
//! that descriptor's plugin will read at resolve time -- for blobstore,
//! `endpoint`, `container` and `prefix`, exactly the fields
//! `wamn_blobstore::binding::resolve` demands. A definition missing one, or
//! carrying one nobody reads, is refused by name before any connection is
//! opened. The requirement being bound must be of the same type: binding a
//! blobstore instance to an alias a component declared as HTTP is refused too.
//!
//! `validation_hash` names WHAT WAS VALIDATED: the requirement's hash, the
//! definition's hash, and the descriptor's type and contract, hashed with the
//! same function as the definition. Nothing else in the tree writes this
//! column today; the plugin's authorization carries it through and checks the
//! surrounding facts, so its role is to make a later re-validation detectable
//! rather than to gate resolution.

use std::path::PathBuf;

use anyhow::{Context as _, bail, ensure};
use clap::Args;
use serde_json::Value;
use tokio_postgres::NoTls;
use wamn_catalog::ConnectionTypeDescriptor;
use wamn_runtime::connection_generation::definition_hash;
use wamn_schema_control::connections::{
    activate_connection_generation_sql, insert_component_connection_binding_sql,
    insert_connection_generation_sql, insert_connection_instance_sql,
};

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";
const SELECT_REQUIREMENT_SQL: &str = "\
SELECT requirement_json::text, requirement_hash FROM catalog.connection_requirements \
 WHERE tenant_id = $1 AND component_digest = $2 AND store_alias = $3";
const FIRST_GENERATION: i64 = 1;

/// The one connection type this verb can bind today. The enum is the CLI's
/// closed vocabulary: a type not listed here cannot be named on the command
/// line, so a descriptor is never authored from a string.
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum RequirementType {
    Blobstore,
}

impl RequirementType {
    fn descriptor(self) -> ConnectionTypeDescriptor {
        match self {
            Self::Blobstore => ConnectionTypeDescriptor::blobstore_v1(),
        }
    }

    /// The coordinates the type's plugin reads from a generation definition.
    /// For blobstore these are the three `wamn_blobstore::binding::resolve`
    /// demands, and nothing else: a key nobody reads is a key nobody validates.
    fn coordinates(self) -> &'static [&'static str] {
        match self {
            Self::Blobstore => &["endpoint", "container", "prefix"],
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
    pub requirement_type: RequirementType,

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

/// What the verb wrote, for the caller's receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundConnection {
    pub instance_id: String,
    pub generation: i64,
    pub definition_hash: String,
    pub validation_hash: String,
}

/// Check a definition against the descriptor's coordinates. Pure; the CLI's
/// refusal and the proof's controls both go through here.
pub fn validate_definition(
    requirement_type: RequirementType,
    definition: &Value,
) -> anyhow::Result<()> {
    let Some(object) = definition.as_object() else {
        bail!("the generation definition must be a JSON object");
    };
    for coordinate in requirement_type.coordinates() {
        match object.get(*coordinate) {
            Some(Value::String(value)) if !value.is_empty() => {}
            Some(Value::String(_)) => bail!(
                "the generation definition's {coordinate} is empty; {:?} needs it",
                requirement_type
            ),
            Some(_) => bail!(
                "the generation definition's {coordinate} must be a string; {:?} reads it as one",
                requirement_type
            ),
            None => bail!(
                "the generation definition lacks {coordinate}; {:?} reads it at resolve time",
                requirement_type
            ),
        }
    }
    for key in object.keys() {
        ensure!(
            requirement_type.coordinates().contains(&key.as_str()),
            "the generation definition carries {key}, which {:?} never reads; \
             a coordinate nobody reads is a coordinate nobody validates",
            requirement_type
        );
    }
    Ok(())
}

/// The bytes `validation_hash` is the hash of, so a reader can name them.
pub fn validation_subject(
    descriptor: &ConnectionTypeDescriptor,
    requirement_hash: &str,
    definition_hash: &str,
) -> Value {
    serde_json::json!({
        "requirement-type": descriptor.requirement_type,
        "contract": descriptor.contract,
        "requirement-hash": requirement_hash,
        "definition-hash": definition_hash,
    })
}

pub async fn run(args: BindConnectionArgs) -> anyhow::Result<()> {
    let bound = bind(&args).await?;
    println!(
        "bound {}:{} to {} generation {} in release {} (definition {}, validation {})",
        args.component_digest,
        args.store_alias,
        bound.instance_id,
        bound.generation,
        args.effective_release_id,
        bound.definition_hash,
        bound.validation_hash
    );
    Ok(())
}

pub async fn bind(args: &BindConnectionArgs) -> anyhow::Result<BoundConnection> {
    let definition_bytes = std::fs::read(&args.definition)
        .with_context(|| format!("read the generation definition {}", args.definition.display()))?;
    let definition: Value = serde_json::from_slice(&definition_bytes)
        .with_context(|| format!("{} is not JSON", args.definition.display()))?;
    validate_definition(args.requirement_type, &definition)?;
    let descriptor = args.requirement_type.descriptor();
    ensure!(
        !args.credential_handle.is_empty(),
        "the credential handle must not be empty; the host resolves it by name"
    );

    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to the project-environment database")?;
    let connection_task = tokio::spawn(connection);
    let result = bind_in(&mut client, args, &descriptor, &definition).await;
    drop(client);
    connection_task
        .await
        .context("join the bind-connection connection")?
        .context("drive the bind-connection connection")?;
    result
}

async fn bind_in(
    client: &mut tokio_postgres::Client,
    args: &BindConnectionArgs,
    descriptor: &ConnectionTypeDescriptor,
    definition: &Value,
) -> anyhow::Result<BoundConnection> {
    let transaction = client
        .transaction()
        .await
        .context("open the bind-connection transaction")?;
    transaction
        .execute(CLAIM_TENANT_SQL, &[&args.tenant])
        .await
        .context("claim the tenant for the bind-connection transaction")?;

    // The requirement being bound must exist and must be of this type: a
    // binding is a claim about a component's declared alias, and the plugin
    // checks the requirement's own descriptor against the instance's at
    // resolve time. Refuse here, naming both, rather than store a pair the
    // plugin will reject.
    let requirement = transaction
        .query_opt(
            SELECT_REQUIREMENT_SQL,
            &[&args.tenant, &args.component_digest, &args.store_alias],
        )
        .await
        .context("read the component's connection requirement")?;
    let Some(requirement) = requirement else {
        bail!(
            "component {} declares no connection requirement named {}; push-component records \
             one per declared alias and this binds only what was declared",
            args.component_digest,
            args.store_alias
        );
    };
    let requirement_json: String = requirement.get(0);
    let requirement_hash: String = requirement.get(1);
    let requirement: Value =
        serde_json::from_str(&requirement_json).context("parse the stored requirement")?;
    // ComponentConnectionRequirement serializes kebab-case; this is the path
    // the plugin's own authorization reads at resolve time.
    let declared_type = requirement["requirement"]["requirement-type"].as_str();
    let declared_contract = requirement["requirement"]["contract"].as_str();
    ensure!(
        declared_type == Some(descriptor.requirement_type.as_str())
            && declared_contract == Some(descriptor.contract.as_str()),
        "component {} declared {} as {}/{}, not {}/{}; a {:?} instance cannot be bound to it",
        args.component_digest,
        args.store_alias,
        declared_type.unwrap_or("?"),
        declared_contract.unwrap_or("?"),
        descriptor.requirement_type,
        descriptor.contract,
        args.requirement_type
    );

    let definition_digest = definition_hash(definition);
    let validation_digest = definition_hash(&validation_subject(
        descriptor,
        &requirement_hash,
        &definition_digest,
    ));
    let definition_text =
        serde_json::to_string(definition).context("serialize the generation definition")?;

    transaction
        .execute(
            insert_connection_instance_sql(),
            &[
                &args.tenant,
                &args.environment,
                &args.instance_id,
                &descriptor.requirement_type,
                &descriptor.contract,
            ],
        )
        .await
        .context("insert the connection instance")?;
    transaction
        .execute(
            insert_connection_generation_sql(),
            &[
                &args.tenant,
                &args.environment,
                &args.instance_id,
                &FIRST_GENERATION,
                &definition_text,
                &definition_digest,
                &args.credential_handle,
            ],
        )
        .await
        .context("insert the connection generation")?;
    // Activation advances the instance's revision: the schema's own guard
    // refuses an update that does not, so the builder is the library's.
    let activated = transaction
        .execute(
            activate_connection_generation_sql(),
            &[
                &args.tenant,
                &args.environment,
                &args.instance_id,
                &FIRST_GENERATION,
            ],
        )
        .await
        .context("activate the connection generation")?;
    ensure!(
        activated == 1,
        "activating generation {FIRST_GENERATION} touched {activated} instance rows, not one"
    );
    let release_id = i32::try_from(args.effective_release_id)
        .context("the effective release id does not fit the catalog's int column")?;
    transaction
        .execute(
            insert_component_connection_binding_sql(),
            &[
                &args.tenant,
                &release_id,
                &args.component_digest,
                &args.store_alias,
                &args.environment,
                &args.instance_id,
                &"active",
                &"valid",
                &validation_digest,
            ],
        )
        .await
        .context("insert the component connection binding")?;
    transaction
        .commit()
        .await
        .context("commit the bind-connection transaction")?;

    Ok(BoundConnection {
        instance_id: args.instance_id.clone(),
        generation: FIRST_GENERATION,
        definition_hash: definition_digest,
        validation_hash: validation_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blobstore(definition: Value) -> anyhow::Result<()> {
        validate_definition(RequirementType::Blobstore, &definition)
    }

    #[test]
    fn a_blobstore_definition_needs_exactly_the_coordinates_the_plugin_reads() {
        blobstore(serde_json::json!({
            "endpoint": "http://10.0.0.7:9000", "container": "labels", "prefix": "wms/"
        }))
        .expect("the three coordinates the plugin reads are a complete definition");
    }

    #[test]
    fn a_missing_coordinate_is_refused_by_name() {
        let error = blobstore(serde_json::json!({"endpoint": "http://x", "container": "c"}))
            .expect_err("prefix is read at resolve time");
        assert!(format!("{error:#}").contains("lacks prefix"), "{error:#}");
    }

    #[test]
    fn an_empty_coordinate_is_refused_by_name() {
        let error = blobstore(serde_json::json!({"endpoint": "", "container": "c", "prefix": "p"}))
            .expect_err("an empty endpoint is not an endpoint");
        assert!(format!("{error:#}").contains("endpoint is empty"), "{error:#}");
    }

    #[test]
    fn a_coordinate_nobody_reads_is_refused_by_name() {
        let error = blobstore(serde_json::json!({
            "endpoint": "http://x", "container": "c", "prefix": "p", "region": "eu-3"
        }))
        .expect_err("a key the plugin never reads is a key nobody validates");
        assert!(format!("{error:#}").contains("carries region"), "{error:#}");
    }

    #[test]
    fn the_http_shape_is_not_a_blobstore_definition() {
        // The control library's ConnectionGenerationDefinition is the HTTP
        // shape. Handing it to a blobstore descriptor is the mismatch the
        // ruling made unconstructible.
        let error = blobstore(serde_json::json!({
            "primary-authority": "https://erp.example", "failover-authorities": [],
            "tls-policy": "verify-authority", "redirect-policy": "same-authority"
        }))
        .expect_err("an HTTP definition has none of blobstore's coordinates");
        assert!(format!("{error:#}").contains("lacks endpoint"), "{error:#}");
    }

    #[test]
    fn a_non_object_definition_is_refused() {
        let error = blobstore(serde_json::json!(["endpoint"])).expect_err("not an object");
        assert!(format!("{error:#}").contains("must be a JSON object"), "{error:#}");
    }

    #[test]
    fn the_validation_subject_names_everything_that_was_validated() {
        let descriptor = ConnectionTypeDescriptor::blobstore_v1();
        let subject = validation_subject(&descriptor, "sha256:req", "sha256:def");
        assert_eq!(subject["requirement-type"], "blobstore");
        assert_eq!(subject["contract"], "wasmcloud:blobstore/blobstore@0.1.0");
        assert_eq!(subject["requirement-hash"], "sha256:req");
        assert_eq!(subject["definition-hash"], "sha256:def");
        // One hasher: the runtime's. Changing any named input changes the hash.
        let baseline = definition_hash(&subject);
        let moved = definition_hash(&validation_subject(&descriptor, "sha256:req", "sha256:other"));
        assert_ne!(baseline, moved);
        assert!(baseline.starts_with("sha256:") && baseline.len() == 71);
    }
}
