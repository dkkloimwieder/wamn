//! Project reservations, credentials, and database grants.

mod output;
mod plan;
pub(crate) mod registry;
pub(crate) mod workload;

pub use output::{ensure_secret_path, write_secret_json};
pub use plan::{privilege_sql, role_posture_sql};
pub use registry::{read_project_env_instance, require_connected};
pub use wamn_platform_install::transaction::provisioning_transaction;

#[cfg(test)]
mod tests;
