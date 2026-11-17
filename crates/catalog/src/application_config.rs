//! Application facts supplied to host plugins through workload configuration.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The route component's application configuration key.
pub const APPLICATION_CONFIG: &str = "wamn.application";

/// Deploy facts shared by an application's host plugins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApplicationConfig {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub environment: wamn_registry::EnvName,
    pub application: crate::ApplicationName,
    /// One positive grant cache lifetime for the project, in seconds.
    pub grant_cache_seconds: u32,
    pub components: Vec<ApplicationComponent>,
}

/// One component's dispatch identity, SQL schema and operation policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApplicationComponent {
    /// The sibling component name in the WorkloadDeployment.
    pub name: String,
    pub package: crate::PackageName,
    /// Absent for a component that declares no SQL.
    pub schema: Option<String>,
    pub operations: BTreeMap<crate::canonical_name::CanonicalName, ApplicationOperation>,
}

/// The host policy of one component export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApplicationOperation {
    /// The sibling component name used for dispatch in the WorkloadDeployment.
    pub component: String,
    pub public: bool,
    pub host_transaction: bool,
    /// SQL admission mode, preserving delete as a distinct operation type.
    /// Absent for exports without a package operation contract (such as palette nodes).
    #[serde(rename = "type")]
    pub type_: Option<ApplicationOperationType>,
    pub idempotency_key: Option<String>,
}

/// The SQL admission type of an application operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplicationOperationType {
    Read,
    Write,
    Delete,
}
