//! The applications an environment declares (docs/plan/platform-config.md).
//!
//! `environment.k` names each application and the packages of the release
//! that belong to it. `wamn-ctl env apply` checks the map against the
//! release, renders it for the host beside the tenant and environment, and
//! keeps it with the Helm revision. A workflow belongs to one declared
//! application, by name.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// One declared application: the packages of the release that belong to it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationDefinition {
    pub packages: BTreeSet<String>,
}

/// Application name to its definition.
pub type EnvironmentApplications = BTreeMap<String, ApplicationDefinition>;
