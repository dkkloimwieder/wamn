//! One-shot control-plane verbs.
//!
//! MVP outcome: provisioning · publish · additive schema · tenant isolation (T1 minting).
//!
//! Provisioning (`provision-project`, `provision-org`, `provision-project-env`,
//! `enable-cdc-project-env`), catalog application (`publish-catalog`,
//! `migrate-catalog`), and reconciliation ship in `wamn-ctl`. Environment
//! lifecycle and reporting verbs require the `ops` feature and ship in the
//! separate `wamn-ctl-ops` binary.

#[cfg(feature = "ops")]
pub mod copy_project_env;
#[cfg(feature = "ops")]
pub mod dump_project_env;
pub mod enable_cdc_project_env;
mod env_policies;
#[cfg(feature = "ops")]
pub mod impact_report;
pub mod migrate_catalog;
#[cfg(feature = "ops")]
mod ops_schema;
pub mod provision;
pub mod provision_org;
pub mod provision_project_env;
#[cfg(feature = "ops")]
pub mod prune_run_history;
pub mod publish_catalog;
pub mod publish_release;
pub mod push_component;
pub mod reconcile_replica_identity;
pub mod reconcile_run_plane;
#[cfg(feature = "ops")]
pub mod restore_project_env;
pub mod terminalize_effect_uncertain;

/// Exact literal every closed definition-publish path returns (wamn-0h0g.8.18).
///
/// The management authoring and report store moved to the control database, so a
/// project-database definition publish would write a release nothing reads. Both
/// closed verbs refuse with this one literal **before any network, database, or
/// filesystem I/O**, so a refusal cannot be mistaken for a partial publish.
///
/// The bead it names is the replacement publish path, not this closure: the
/// original ruling named `.8.19`/`.8.22`, and the deployment-simplification
/// amendment (rulings wamn-0h0g.13.43/.13.44) superseded both and re-anchored the
/// literal on wamn-0h0g.15.14. Data-only `copy-project-env` is unaffected.
pub const CONTROL_DEFINITION_PUBLISH_REFUSAL: &str =
    "control-definition-publish-requires-wamn-0h0g.15.14";
