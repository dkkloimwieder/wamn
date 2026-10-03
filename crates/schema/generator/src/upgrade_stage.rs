//! Package-carried declarations for online upgrade stages.
//!
//! The package version identifies the stage. Its manifest binds the exact SQL
//! and conditions. This vocabulary grants no permission to execute a stage.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{GenerateError, GenerateErrorType, PackageManifest};

/// One stage applied by one immutable package version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeStage {
    pub phase: UpgradeStagePhase,
    /// Named SQL predicates evaluated in lexical name order before the stage.
    pub preconditions: BTreeMap<String, String>,
    /// Named SQL predicates evaluated in lexical name order before completion.
    pub postconditions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backfill: Option<BackfillStage>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub exceptions: BTreeSet<UpgradeStageException>,
}

/// The compatibility phase of a package upgrade stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeStagePhase {
    Expand,
    Backfill,
    Contract,
}

/// An explicit exception that requires its own qualification proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeStageException {
    WholeRowGrants,
}

/// SQL and cursor for resumable batches in separate transactions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackfillStage {
    /// SQL takes `$1::jsonb` cursor and `$2::int4` batch size.
    /// It returns one row with `next_cursor jsonb` and `complete bool`.
    pub sql: String,
    pub batch_size: u32,
    /// Explicit initial JSON value, including null. Omission is refused.
    #[serde(deserialize_with = "required_cursor")]
    pub initial_cursor: Value,
}

fn required_cursor<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Value, D::Error> {
    Value::deserialize(deserializer)
}

/// Validate the closed stage vocabulary independently of its package.
///
/// # Errors
/// Returns a contextual manifest refusal for incomplete or incompatible declarations.
pub fn validate_upgrade_stage(stage: &UpgradeStage) -> Result<(), GenerateError> {
    for (section, conditions) in [
        ("preconditions", &stage.preconditions),
        ("postconditions", &stage.postconditions),
    ] {
        if conditions.is_empty() {
            return Err(refusal(
                section,
                "must contain at least one named SQL condition",
            ));
        }
        for (name, sql) in conditions {
            if name.trim().is_empty() || sql.trim().is_empty() {
                return Err(refusal(
                    &format!("{section}.{name}"),
                    "condition name and SQL must not be empty",
                ));
            }
        }
    }
    match (stage.phase, &stage.backfill) {
        (UpgradeStagePhase::Backfill, Some(batch)) => {
            if batch.sql.trim().is_empty() {
                return Err(refusal("backfill.sql", "must not be empty"));
            }
            if batch.batch_size == 0 || i32::try_from(batch.batch_size).is_err() {
                return Err(refusal(
                    "backfill.batch_size",
                    "must be between 1 and 2147483647",
                ));
            }
        }
        (UpgradeStagePhase::Backfill, None) => {
            return Err(refusal("backfill", "is required for the backfill phase"));
        }
        (_, Some(_)) => return Err(refusal("backfill", "requires the backfill phase")),
        (_, None) => {}
    }
    if !stage.exceptions.is_empty() && stage.phase != UpgradeStagePhase::Expand {
        return Err(refusal(
            "exceptions",
            "whole_row_grants requires the expand phase",
        ));
    }
    Ok(())
}

pub(crate) fn validate_package_upgrade_stage(
    manifest: &PackageManifest,
) -> Result<(), GenerateError> {
    let Some(stage) = &manifest.upgrade_stage else {
        return Ok(());
    };
    validate_upgrade_stage(stage)?;
    if manifest
        .package
        .predecessor_version
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err(refusal("phase", "requires a package predecessor version"));
    }
    if manifest.declares_no_sql() {
        return Err(refusal("phase", "requires a SQL-bearing package"));
    }
    Ok(())
}

fn refusal(field: &str, message: &str) -> GenerateError {
    GenerateError::for_object(
        GenerateErrorType::InvalidManifest,
        format!("upgrade_stage.{field} {message}"),
        format!("upgrade_stage.{field}"),
    )
}
