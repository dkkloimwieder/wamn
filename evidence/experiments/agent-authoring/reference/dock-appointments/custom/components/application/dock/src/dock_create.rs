//! `dock.create` -- mint one dock under an idempotency key.
//!
//! The identity comes from the CLAIM, never from the work: `claim_command`
//! pre-generates `dock_id` under `idempotency_key PRIMARY KEY`, and the
//! insert BINDS it. A replay therefore returns the same id by construction.

use serde::Deserialize;
use wamn_postgres_statements::{Connection, Transaction};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::dock_create as sql;
use crate::scalar;

pub(crate) const REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::IdempotencyConflict,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateDockCommand {
    pub(crate) idempotency_key: String,
    pub(crate) name: String,
}

/// The bytes the idempotency key keys: the RE-SPELLED command. The key itself
/// is excluded, because two deliveries of one operator action differ in the
/// envelope that carried them and in nothing else.
fn canonical_command(name: &str) -> Vec<u8> {
    wamn_execution_contract::canonical_json_bytes(&serde_json::json!({ "name": name }))
}

/// # Errors
///
/// [`AccessError`] carrying the literal and detail the operation contract
/// declares for that refusal.
pub(crate) async fn execute(command: &CreateDockCommand) -> Result<serde_json::Value, AccessError> {
    let name = scalar::name("value.name", &command.name)?;
    let canonical = canonical_command(&name);

    let mut connection = Connection::new();
    let mut transaction = connection
        .begin()
        .await
        .map_err(|e| error::from_statement(&e))?;
    match run(&mut transaction, command, &name, &canonical).await {
        Ok(dock_id) => {
            transaction
                .commit()
                .await
                .map_err(|e| error::from_statement(&e))?;
            Ok(serde_json::json!({ "dock_id": dock_id }))
        }
        Err(refusal) => {
            let _ = transaction.rollback().await;
            Err(refusal)
        }
    }
}

async fn run(
    transaction: &mut Transaction,
    command: &CreateDockCommand,
    name: &str,
    canonical: &[u8],
) -> Result<String, AccessError> {
    // A REPLAY RETURNS THE ORIGINAL RESULT, unchanged, and writes nothing.
    if let Some(replay) = sql::find_replay(transaction, command.idempotency_key.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
    {
        if replay.canonical_command != canonical {
            return Err(AccessError::field(
                AccessErrorKind::IdempotencyConflict,
                "value.idempotency_key",
            ));
        }
        if replay.finalized.is_none() {
            // Claimed but never finalized: the first attempt died between the
            // claim and the commit. Retryable under the same key.
            return Err(AccessError::retry());
        }
        return Ok(replay.dock_id.0.clone());
    }

    let claim = sql::claim_command(
        transaction,
        command.idempotency_key.clone(),
        canonical.to_vec(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(AccessError::retry)?;

    sql::insert_dock(transaction, claim.dock_id.clone(), name.to_owned())
        .await
        .map_err(|e| error::from_statement(&e))?;

    let finalized = sql::finalize_command(
        transaction,
        command.idempotency_key.clone(),
        canonical.to_vec(),
        claim.dock_id.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    if finalized.finalized.is_none() {
        return Err(AccessError::retry());
    }

    Ok(claim.dock_id.0.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key is not part of what the key identifies: two deliveries of one
    /// creation under different keys still describe the same dock, and the
    /// same key under a different name is a different command.
    #[test]
    fn the_canonical_command_excludes_the_key_and_separates_names() {
        let here = canonical_command("North Bay");
        assert_eq!(here, canonical_command("North Bay"));
        assert_ne!(here, canonical_command("South Bay"));
        assert!(!String::from_utf8(here).unwrap().contains("idempotency_key"));
    }
}
