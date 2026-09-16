//! The control library that `services/ctl`, the dev loop, and test support call.
//!
//! It holds the admission, release, provisioning, reconcile, and package
//! operations. It does database, filesystem, and network work, and it runs the
//! processes that work needs. It does no CLI presentation and does not own the
//! CLI lifecycle: exit codes, signals, and stdout belong to `services/ctl`.

pub mod apply_package;
pub mod author_wiring;
pub mod component_declaration;
pub mod enable_cdc_project_env;
pub mod env_policies;
#[cfg(feature = "ops")]
pub mod event_advisories;
pub mod event_streams;
pub mod ident;
pub mod identity_issuer;
pub mod pat_client;
pub mod project_env_membership;
pub mod promote;
pub mod provision_project_env;
#[cfg(feature = "ops")]
pub mod prune_record_history;
#[cfg(feature = "ops")]
pub mod prune_run_history;
pub mod publish_release;
pub mod push_component;
pub mod reconcile_package_data_access;
pub mod reconcile_replica_identity;
pub mod reconcile_run_plane;
pub mod sql_params;
pub mod terminalize_effect_uncertain;
pub mod verification_policy;
