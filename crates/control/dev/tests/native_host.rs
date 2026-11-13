//! The native host over private PostgreSQL and NATS.
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};
use wamn_dev_control::dev::environment::connect;
use wamn_registry::Triple;
use wamn_release_control::publish_release::{PublishedRelease, record_composed_entry};
#[path = "../../environment/tests/support/release_documents.rs"]
pub mod release_documents;
use release_documents::{
    MANIFEST_H, NAMESPACE, ORG, PACKAGE, PLATFORM_DOMAIN, POLICY, PROJECT, fixture_document,
    fixture_host_group, fixture_http_entry, release,
};
use release_documents::{http as http_connection, workflow as integration};
#[path = "../../environment/tests/support/prekind_host.rs"]
mod prekind_host;

const PASSWORD: &str = "apply-password";
const ROUTE_NOT_FOUND: &str = r#"{"error":{"code":"route-not-found"}}"#;
