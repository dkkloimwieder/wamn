//! The edge release bundle: the files that publish writes for an edge box, and
//! the one file whose digest pins them (docs/history/edge.md section 4.6).
//!
//! The directory holds `edge-release.json`, the canonical serving manifest,
//! `components.json`, `grants.json`, `flow-http.wasm`, and `<sha256>.wasm` for
//! each component. `flow-http.wasm` is the composed entry: the http-route guest
//! composed with the release's application. The writer is `wamn dev
//! edge-bundle`, and the reader is `wamn-edge`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The bundle file name inside a release bundle directory.
pub const BUNDLE_FILE_NAME: &str = "edge-release.json";
/// The component facts file name inside a release bundle directory.
pub const COMPONENTS_FILE_NAME: &str = "components.json";
/// The grants file name inside a release bundle directory.
pub const GRANTS_FILE_NAME: &str = "grants.json";
/// The composed entry file name inside a release bundle directory.
pub const INGRESS_FILE_NAME: &str = "flow-http.wasm";
/// The one bundle format that the writer writes and the edge reads.
pub const BUNDLE_FORMAT: u32 = 1;

/// `edge-release.json`: the format and the digest of each pinned file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeBundle {
    pub format: u32,
    pub manifest: String,
    pub components: String,
    pub grants: String,
    pub ingress: String,
}

/// `grants.json`: the permissions of each role on the box.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeGrants {
    pub roles: BTreeMap<String, BTreeSet<String>>,
}
