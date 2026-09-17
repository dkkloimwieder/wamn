//! Portable connection requirements and environment-owned persistence records.

use sha2::{Digest as _, Sha256};

#[doc(inline)]
pub use wamn_catalog::ComponentConnectionRequirement;

/// Insert one immutable component requirement into the PROJECT plane; identical
/// retries converge.
///
/// # The two planes no longer share one statement (wamn-10yt.52)
///
/// The CONTROL copy of `catalog.connection_requirements` grew an
/// `environment_instance` key column and the project copy did not: a project
/// database is dropped and cloned before every development run, so it has no
/// second creation to tell itself apart from. One statement can no longer serve
/// both planes, so the control plane gets its own pair below rather than the
/// project plane carrying a column it has no use for.
pub fn insert_component_connection_requirement_sql() -> &'static str {
    "INSERT INTO catalog.connection_requirements \
       (tenant_id, component_digest, store_alias, requirement_json, requirement_hash) \
     VALUES ($1, $2, $3, $4::text::jsonb, $5) \
     ON CONFLICT DO NOTHING"
}

/// Check an existing PROJECT-plane component requirement row is byte-identical
/// to this one.
///
/// The parameters are exactly [`insert_component_connection_requirement_sql`]'s,
/// so a writer whose insert converged away can tell whether it converged onto
/// its own record or collided with a different one at the same coordinate.
pub fn exact_component_connection_requirement_sql() -> &'static str {
    "SELECT EXISTS (\
       SELECT 1 FROM catalog.connection_requirements \
        WHERE tenant_id = $1 AND component_digest = $2 AND store_alias = $3 \
          AND requirement_json = $4::text::jsonb AND requirement_hash = $5\
     )"
}

/// The CONTROL plane's same append, keyed additionally by the environment
/// instance at `$2` (wamn-10yt.52).
///
/// `$2` is the tenant's current creation of the project database, or the empty
/// string for an environment nothing recreates. Every other parameter keeps
/// [`insert_component_connection_requirement_sql`]'s position and meaning, so the
/// two statements differ by exactly the one part that is new.
pub fn insert_control_component_connection_requirement_sql() -> &'static str {
    "INSERT INTO catalog.connection_requirements \
       (tenant_id, environment_instance, component_digest, store_alias, \
        requirement_json, requirement_hash) \
     VALUES ($1, $2, $3, $4, $5::text::jsonb, $6) \
     ON CONFLICT DO NOTHING"
}

/// Check an existing CONTROL-plane component requirement row is byte-identical
/// to this one, WITHIN this environment instance.
///
/// The parameters are exactly
/// [`insert_control_component_connection_requirement_sql`]'s. Without the
/// instance in the predicate, a rerun would read the PREVIOUS creation's row and
/// mistake it for its own record.
pub fn exact_control_component_connection_requirement_sql() -> &'static str {
    "SELECT EXISTS (\
       SELECT 1 FROM catalog.connection_requirements \
        WHERE tenant_id = $1 AND environment_instance = $2 \
          AND component_digest = $3 AND store_alias = $4 \
          AND requirement_json = $5::text::jsonb AND requirement_hash = $6\
     )"
}

/// Insert one environment-owned stable instance identity.
pub fn insert_connection_instance_sql() -> &'static str {
    "INSERT INTO catalog.connection_instances \
       (tenant_id, environment, instance_id, requirement_type, contract) \
     VALUES ($1, $2, $3, $4, $5)"
}

/// Insert one immutable generation; secret material is represented only by a handle.
pub fn insert_connection_generation_sql() -> &'static str {
    "INSERT INTO catalog.connection_generations \
       (tenant_id, environment, instance_id, generation, definition_json, \
        definition_hash, credential_set_handle) \
     VALUES ($1, $2, $3, $4, $5::text::jsonb, $6, $7)"
}

/// Activate one generation on its instance, but only while the instance is
/// still in the state the caller read. `$5` is the expected `active_generation`
/// (NULL when none is active) and `$6` is the expected `revision`. A stale
/// expectation matches no row, so the caller sees 0 rows and the instance is
/// unchanged. The instance-update guard requires every update to advance
/// `revision`, so activation is a revision, not an edit: the row's identity
/// columns are immutable and the trigger refuses a stale revision with
/// `connection-instance-revision-must-advance`.
pub fn activate_connection_generation_sql() -> &'static str {
    "UPDATE catalog.connection_instances \
        SET active_generation = $4, revision = revision + 1 \
      WHERE tenant_id = $1 AND environment = $2 AND instance_id = $3 \
        AND active_generation IS NOT DISTINCT FROM $5 AND revision = $6"
}

/// Read the active definition and the exact instance state used by activation.
pub fn select_connection_instance_sql() -> &'static str {
    "SELECT instance.requirement_type, instance.contract, instance.lifecycle_status, \
            instance.active_generation, instance.revision, generation.definition_json::text, \
            generation.definition_hash, generation.credential_set_handle, \
            (SELECT max(generation) FROM catalog.connection_generations \
              WHERE tenant_id = $1 AND environment = $2 AND instance_id = $3) \
       FROM catalog.connection_instances AS instance \
       LEFT JOIN catalog.connection_generations AS generation \
         ON generation.tenant_id = instance.tenant_id \
        AND generation.environment = instance.environment \
        AND generation.instance_id = instance.instance_id \
        AND generation.generation = instance.active_generation \
      WHERE instance.tenant_id = $1 AND instance.environment = $2 AND instance.instance_id = $3"
}

/// Hold an unchanged selection until an identical binding transaction commits.
pub fn lock_connection_selection_sql() -> &'static str {
    "SELECT 1 FROM catalog.connection_instances \
      WHERE tenant_id = $1 AND environment = $2 AND instance_id = $3 \
        AND active_generation IS NOT DISTINCT FROM $4 AND revision = $5 \
      FOR UPDATE"
}

/// Insert one immutable component release binding to an environment instance.
pub fn insert_component_connection_binding_sql() -> &'static str {
    "INSERT INTO catalog.connection_bindings \
       (tenant_id, effective_release_id, component_digest, store_alias, \
        environment, instance_id, binding_status, validation_status, validation_hash) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
}

/// The `sha256:<hex>` identity of `bytes`. The package manifest digest and the
/// per-migration digest both use this one copy.
pub(crate) fn prefixed_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity("sha256:".len() + digest.len() * 2);
    out.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("writing to a string is infallible");
    }
    out
}
