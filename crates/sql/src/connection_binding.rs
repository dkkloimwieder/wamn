//! Resolve the authority of one admitted invocation before an external call.

use crate::{ConnectionEffectLookup, ConnectionEffectSnapshot, WamnPostgres};
use wamn_connections::connection::{
    ConnectionExecutionClosure, authorize_candidate_closure, authorize_release_closure,
    entry_lookup,
};

/// Resolve the released binding of an admitted invocation.
pub async fn resolve(
    postgres: &WamnPostgres,
    scope: &str,
    alias: &str,
) -> anyhow::Result<ConnectionEffectSnapshot> {
    use anyhow::Context as _;
    let context = wamn_connections::invocation::context(&postgres.scopes, scope)?;
    postgres
        .scopes
        .read::<crate::invocation::Authority, _>(scope, |record| {
            crate::invocation::validate_context(&record.identity, &context)
        })
        .context("scope has no SQL identity")??;
    let facts = &context.connections;
    let release = &context.release;
    let invocation = postgres
        .invocation(scope)
        .context("no admitted connection invocation")?;
    anyhow::ensure!(
        &release.target().project_env_key == facts.project_env_key(),
        "connection release target mismatch"
    );
    let (digest, environment, candidate) = match &invocation.closure {
        ConnectionExecutionClosure::Released => (
            &release.release().manifest_digest,
            &release.target().environment,
            None,
        ),
        ConnectionExecutionClosure::Candidate {
            manifest_digest,
            environment,
            binding_world,
            ..
        } => {
            anyhow::ensure!(
                *manifest_digest == release.release().manifest_digest
                    && environment == &release.target().environment,
                "admitted invocation release mismatch"
            );
            (
                manifest_digest,
                environment,
                Some(
                    binding_world
                        .binding(&invocation.component_digest, alias)
                        .context("admission has no binding for this alias")?,
                ),
            )
        }
    };
    let entry =
        entry_lookup(&invocation).map_err(|_| anyhow::anyhow!("invalid invocation entry"))?;
    let snapshot = postgres
        .connection_effect_snapshot(
            scope,
            facts.project_env_key(),
            &ConnectionEffectLookup {
                package_id: &invocation.package_id,
                entry,
                origin_package_id: &invocation.origin.package_id,
                origin_component_digest: &invocation.origin.component_digest,
                origin_component: &invocation.origin.component,
                origin_interface_version: &invocation.origin.interface_version,
                origin_operation: &invocation.origin.operation,
                operation: &invocation.operation,
                manifest_digest: digest,
                environment,
                component_digest: &invocation.component_digest,
                store_alias: alias,
                candidate_binding: candidate,
            },
            facts,
            release,
        )
        .await?
        .context("connection binding unavailable")?;
    let authorized = match candidate {
        Some(candidate) => authorize_candidate_closure(&invocation, &snapshot, candidate),
        None => authorize_release_closure(release.manifest(), &invocation, &snapshot),
    };
    authorized.map_err(|_| anyhow::anyhow!("connection invocation closure refused"))?;
    Ok(snapshot)
}
