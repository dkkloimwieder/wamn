use std::collections::BTreeSet;

use wamn_connection_type::RuntimeType;
use wamn_digest::Digest;
use wamn_package::manifest::ConnectionMode;
use wamn_registry::ApplicationName;
use wamn_registry::integration_code::IntegrationCode;

use crate::ResolvedConnection;

/// One declared name resolved to an active environment connection and version.
#[derive(Debug, Clone)]
pub struct Binding {
    pub application: ApplicationName,
    pub alias: String,
    pub interface: String,
    pub components: BTreeSet<String>,
    pub runtime_type: RuntimeType,
    pub target_type: String,
    pub connection_id: String,
    pub authority: Option<String>,
    pub integration_type: IntegrationCode,
    pub exclusive: bool,
    pub generation: i64,
    pub mode: ConnectionMode,
    pub digest: Option<Digest>,
    pub exports: Vec<String>,
    pub write_interfaces: Vec<String>,
    pub resolved: Option<ResolvedConnection>,
}
