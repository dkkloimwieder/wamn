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

/// The one connection type this verb can bind today. The enum is the closed
/// vocabulary a caller chooses from, so a descriptor is never authored from
/// a string.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequirementType {
    Blobstore,
}

impl RequirementType {
    pub fn descriptor(self) -> ConnectionTypeDescriptor {
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

#[derive(Debug)]
pub struct BindConnectionRequest {
    /// The project-environment database holding the catalog schema.
    pub database_url: String,

    pub tenant: String,

    pub environment: String,

    /// The environment-owned, stable identity of the connection instance.
    pub instance_id: String,

    /// Which platform descriptor the instance carries. Minted, never authored.
    pub requirement_type: RequirementType,

    /// The generation's non-secret definition: a JSON object of exactly the
    /// coordinates the requirement type's plugin reads.
    pub definition: PathBuf,

    /// The host-held credential's handle. Never the credential.
    pub credential_handle: String,

    /// The release whose component is being bound.
    pub effective_release_id: u32,

    /// The admitted component's digest, as push-component printed it.
    pub component_digest: String,

    /// The store alias that component declared for this connection.
    pub store_alias: String,
}

/// What the verb wrote, for the caller's result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundConnection {
    pub instance_id: String,
    pub generation: i64,
    pub definition_hash: String,
    pub validation_hash: String,
}

/// Check a definition against the descriptor's coordinates. Pure; the CLI's
/// refusal and the test's controls both go through here.
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

#[doc(inline)]
pub use wamn_runtime::connection_generation::binding_validation_subject as validation_subject;

/// Write the instance, its first generation, and the release-scoped binding.
pub async fn bind(args: &BindConnectionRequest) -> anyhow::Result<BoundConnection> {
    let definition_bytes = std::fs::read(&args.definition).with_context(|| {
        format!(
            "read the generation definition {}",
            args.definition.display()
        )
    })?;
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
    args: &BindConnectionRequest,
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
    insert_instance_generation(
        &transaction,
        &args.tenant,
        &args.environment,
        &args.instance_id,
        descriptor,
        definition,
        &args.credential_handle,
    )
    .await?;
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
        assert!(
            format!("{error:#}").contains("endpoint is empty"),
            "{error:#}"
        );
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
        assert!(
            format!("{error:#}").contains("must be a JSON object"),
            "{error:#}"
        );
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
        let moved = definition_hash(&validation_subject(
            &descriptor,
            "sha256:req",
            "sha256:other",
        ));
        assert_ne!(baseline, moved);
        assert!(baseline.starts_with("sha256:") && baseline.len() == 71);
    }
}

/// Existing non-secret provisioning inputs for a disposable local instance.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LocalInstanceInput {
    pub requirement_type: RequirementType,
    pub definition: PathBuf,
    pub credential_handle: String,
}

/// Validated non-secret instance inputs retained until candidate cutover.
#[derive(Debug)]
pub struct PreparedLocalInstance {
    requirement_type: RequirementType,
    definition: Value,
    credential_handle: String,
}

pub fn read_local_instance(
    input: &LocalInstanceInput,
) -> anyhow::Result<PreparedLocalInstance> {
    ensure!(
        input.definition.is_absolute(),
        "local connection definition path must be absolute"
    );
    ensure!(
        !input.credential_handle.is_empty(),
        "local credential handle must not be empty"
    );
    let definition: Value = serde_json::from_slice(
        &std::fs::read(&input.definition).context("read local instance definition")?,
    )?;
    validate_definition(input.requirement_type, &definition)?;
    Ok(PreparedLocalInstance {
        requirement_type: input.requirement_type,
        definition,
        credential_handle: input.credential_handle.clone(),
    })
}

/// Insert only absent local instances; never replace live generation authority.
pub async fn prepare_local_instance(
    client: &impl tokio_postgres::GenericClient,
    tenant: &str,
    environment: &str,
    instance_id: &str,
    input: &PreparedLocalInstance,
) -> anyhow::Result<()> {
    ensure!(
        !instance_id.is_empty(),
        "local instance id must not be empty"
    );
    let definition = &input.definition;
    let descriptor = input.requirement_type.descriptor();
    let current = client.query_opt(
        "SELECT instance.requirement_type, instance.contract, instance.lifecycle_status, instance.active_generation,
                generation.definition_json::text, generation.definition_hash, generation.credential_set_handle
         FROM catalog.connection_instances AS instance
         LEFT JOIN catalog.connection_generations AS generation
           ON generation.tenant_id = instance.tenant_id AND generation.environment = instance.environment
          AND generation.instance_id = instance.instance_id AND generation.generation = instance.active_generation
         WHERE instance.tenant_id = $1 AND instance.environment = $2 AND instance.instance_id = $3",
        &[&tenant, &environment, &instance_id],
    ).await?;
    if let Some(row) = current {
        let text: Option<String> = row.try_get(4)?;
        let existing: Option<Value> = text.as_deref().map(serde_json::from_str).transpose()?;
        ensure!(
            row.try_get::<_, String>(0)? == descriptor.requirement_type
                && row.try_get::<_, String>(1)? == descriptor.contract
                && row.try_get::<_, String>(2)? == "enabled"
                && row.try_get::<_, Option<i64>>(3)?.is_some()
                && existing.as_ref() == Some(definition)
                && row.try_get::<_, Option<String>>(5)?.as_deref()
                    == Some(definition_hash(definition).as_str())
                && row.try_get::<_, Option<String>>(6)?.as_deref()
                    == Some(input.credential_handle.as_str()),
            "local instance inputs differ from live authority; use a distinct instance-id for a changed definition or credential handle"
        );
        return Ok(());
    }
    insert_instance_generation(
        client,
        tenant,
        environment,
        instance_id,
        &descriptor,
        &definition,
        &input.credential_handle,
    )
    .await
}

async fn insert_instance_generation(
    client: &impl tokio_postgres::GenericClient,
    tenant: &str,
    environment: &str,
    instance_id: &str,
    descriptor: &ConnectionTypeDescriptor,
    definition: &Value,
    credential_handle: &str,
) -> anyhow::Result<()> {
    let definition_digest = definition_hash(definition);
    let definition_text =
        serde_json::to_string(definition).context("serialize the generation definition")?;
    client
        .execute(
            insert_connection_instance_sql(),
            &[
                &tenant,
                &environment,
                &instance_id,
                &descriptor.requirement_type,
                &descriptor.contract,
            ],
        )
        .await
        .context("insert the connection instance")?;
    client
        .execute(
            insert_connection_generation_sql(),
            &[
                &tenant,
                &environment,
                &instance_id,
                &FIRST_GENERATION,
                &definition_text,
                &definition_digest,
                &credential_handle,
            ],
        )
        .await
        .context("insert the connection generation")?;
    // Activation advances the instance's revision: the schema's own guard
    // refuses an update that does not, so the builder is the library's. The
    // instance was inserted above in this transaction, so the expected state is
    // no active generation at the first revision.
    let expected_active_generation: Option<i64> = None;
    let expected_revision: i64 = 1;
    let activated = client
        .execute(
            activate_connection_generation_sql(),
            &[
                &tenant,
                &environment,
                &instance_id,
                &FIRST_GENERATION,
                &expected_active_generation,
                &expected_revision,
            ],
        )
        .await
        .context("activate the connection generation")?;
    ensure!(
        activated == 1,
        "activating generation {FIRST_GENERATION} touched {activated} instance rows, not one"
    );
    Ok(())
}
