//! Typed reads and writes of environment placement and project registration.

use crate::{Triple, instance_suffix::InstanceSuffix};

/// Values recorded when provisioning a project deployment.
#[derive(Debug)]
pub struct Registration<'a> {
    pub triple: &'a Triple,
    pub secret_name: &'a str,
    pub secret_namespace: Option<&'a str>,
    pub instance_suffix: &'a InstanceSuffix,
    pub disposable: Option<bool>,
    pub readiness_budget_seconds: i32,
    pub drain_bound_seconds: i32,
    pub target_id: &'a str,
}
