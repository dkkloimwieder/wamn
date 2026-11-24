//! Operation identity and transaction authority stored in invocation scopes.

use crate::{OperationTransaction, PgError};
use wamn_application::session_identity::SessionIdentity;
pub use wamn_application::session_identity::{valid_runner, valid_schema};
use wamn_connection::connection::ConnectionInvocation;
use wamn_connection::invocation::ReleaseContext;
use wamn_engine::invocation_scope::InvocationScopes;
use wamn_event::Causation;
use wamn_postgres::DEFAULT_PROJECT;

pub fn validate_context(
    identity: &SessionIdentity,
    context: &ReleaseContext,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        identity.project_env_key.as_ref()
            == Some(&wamn_registry::name::project_env_key(
                &context.release.org,
                &context.release.project,
                &context.release.environment
            ))
            && identity.project.as_ref() == Some(context.connections.project())
            && identity.application.as_ref() == Some(&context.release.application),
        "invocation application authority mismatch"
    );
    Ok(())
}

/// SQL authority retained through commit and result signing.
#[derive(Debug, Default)]
pub struct Authority {
    pub identity: SessionIdentity,
    pub causation: Option<Causation>,
    pub transaction: Option<OperationTransaction>,
    pub wal_position: Option<String>,
    pub(crate) read: bool,
}

impl wamn_application::invocation_authority::InvocationAuthority for Authority {}

/// Register the project_env_key identity for a component id from workload bind.
pub(crate) fn set_project_env_key(
    scopes: &InvocationScopes,
    component_id: &str,
    project_env_key: &wamn_registry::ProjectEnvKey,
) {
    scopes.update::<Authority, _>(component_id, |record| {
        record.identity.project_env_key = Some(project_env_key.clone());
    });
}

pub(super) fn project_env_key_for(
    scopes: &InvocationScopes,
    component_id: &str,
) -> Option<wamn_registry::ProjectEnvKey> {
    scopes
        .read::<Authority, _>(component_id, |record| {
            record.identity.project_env_key.clone()
        })
        .flatten()
}

/// Register which project's database a component uses. The bench harness
/// calls this directly; the host path feeds it from the `wamn.project`
/// workload config. Absent ⇒ the default project.
pub fn set_project(
    scopes: &InvocationScopes,
    component_id: &str,
    project: &wamn_registry::ProjectId,
) -> anyhow::Result<()> {
    scopes.update::<Authority, _>(component_id, |record| {
        record.identity.project = Some(project.clone());
    });
    Ok(())
}

pub(super) fn project_for(
    scopes: &InvocationScopes,
    component_id: &str,
) -> wamn_registry::ProjectId {
    scopes
        .read::<Authority, _>(component_id, |record| record.identity.project.clone())
        .flatten()
        .unwrap_or_else(|| (*DEFAULT_PROJECT).clone())
}

/// Register the `search_path` schema for a component id. When set, every
/// transaction the plugin opens for that component also runs
/// `SET LOCAL search_path`, so the guest's unqualified table names resolve
/// to a host-chosen schema. The bench harness calls this directly; the host
/// path feeds it from the `wamn.schema` workload config.
pub fn set_schema(
    scopes: &InvocationScopes,
    component_id: &str,
    schema: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_schema(schema),
        "invalid schema {schema:?}: 1-63 chars of [A-Za-z0-9_] starting with a letter/underscore required"
    );
    scopes.update::<Authority, _>(component_id, |record| {
        record.identity.schema = Some(schema.to_string());
    });
    Ok(())
}

/// Register the durable queue lease owner for a component id.
/// Each transaction records it in the protected caller identity.
pub fn set_runner(
    scopes: &InvocationScopes,
    component_id: &str,
    runner: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_runner(runner),
        "invalid runner owner {runner:?}: 1-128 chars of [A-Za-z0-9_-] required"
    );
    scopes.update::<Authority, _>(component_id, |record| {
        record.identity.runner = Some(runner.to_string());
    });
    Ok(())
}

pub(super) fn current_run_for(scopes: &InvocationScopes, component_id: &str) -> Option<Causation> {
    if active_read(scopes, component_id) {
        return None;
    }
    scopes
        .read::<Authority, _>(component_id, |record| record.causation.clone())
        .flatten()
}

/// The invocation currently bound to one pooled instance.
#[must_use]
pub fn invocation(scopes: &InvocationScopes, component_id: &str) -> Option<ConnectionInvocation> {
    wamn_connection::invocation::invocation(scopes, component_id)
}

/// Read back the identity set one component id currently resolves to.
///
/// `None` is the base denial policy: no project_env_key is bound, so
/// [`require_sql_identity`] refuses every call made under
/// this id. That is what an idle, never-acquired execution instance must
/// report, and it is the read half of the checkout-identity seam — the pair
/// this and what [`crate::invocation::AuthorizedInvocation::enter`] writes
/// is what lets a test assert that two concurrent acquisitions never share
/// one identity.
pub fn session_identity(scopes: &InvocationScopes, component_id: &str) -> Option<SessionIdentity> {
    scopes
        .read::<Authority, _>(component_id, |record| record.identity.clone())
        .filter(|identity| identity.project_env_key.is_some())
}

pub(super) fn require_sql_identity(
    scopes: &InvocationScopes,
    component_id: &str,
) -> Result<SessionIdentity, PgError> {
    session_identity(scopes, component_id).ok_or_else(|| {
        tracing::warn!(
            component_id,
            "wamn:postgres call from component with no project_env_key identity"
        );
        PgError::QueryError((
            "WAMN0".to_string(),
            "no project_env_key identity configured for this component".to_string(),
        ))
    })
}

/// Whether the active operation uses a read transaction.
pub(super) fn active_read(scopes: &InvocationScopes, scope: &str) -> bool {
    scopes
        .read::<Authority, _>(scope, |record| record.read)
        .unwrap_or(false)
}

/// Bind `transaction` as the operation transaction of `scope`.
///
/// # Errors
///
/// Fails when `scope` already has one.
pub fn bind_operation_transaction(
    scopes: &InvocationScopes,
    scope: &str,
    transaction: &OperationTransaction,
) -> anyhow::Result<()> {
    scopes
        .update_existing::<Authority, _>(scope, |record| {
            anyhow::ensure!(
                record.transaction.is_none(),
                "operation-transaction-scope-already-bound"
            );
            record.transaction = Some(transaction.clone());
            Ok(())
        })
        .unwrap_or_else(|| Err(anyhow::anyhow!("invocation scope was revoked")))
}

/// Drop the binding of `scope`. The transaction stays open for its owner.
pub fn revoke_operation_transaction(scopes: &InvocationScopes, scope: &str) {
    scopes.update_existing::<Authority, _>(scope, |record| record.transaction = None);
}

pub(super) fn operation_transaction_for(
    scopes: &InvocationScopes,
    scope: &str,
) -> Option<OperationTransaction> {
    scopes
        .read::<Authority, _>(scope, |record| record.transaction.clone())
        .flatten()
}

pub(crate) fn validate_identity(identity: &SessionIdentity) -> anyhow::Result<()> {
    anyhow::ensure!(
        identity.project_env_key.is_some(),
        "project environment identity is absent"
    );
    anyhow::ensure!(
        identity.schema.as_deref().is_none_or(valid_schema),
        "invalid schema"
    );
    anyhow::ensure!(
        identity.runner.as_deref().is_none_or(valid_runner),
        "invalid runner"
    );
    anyhow::ensure!(
        identity.role.as_deref().is_none_or(|role| role
            .parse::<wamn_role::RoleName>()
            .is_ok_and(|parsed| parsed.as_str() == role)),
        "invalid role"
    );
    Ok(())
}
