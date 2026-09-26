//! The edge release bundle: the files that publish writes for an edge box, and
//! the one file whose digest pins them (docs/plan/edge.md section 4.6).
//!
//! The directory holds `edge-release.json`, the canonical serving manifest,
//! `components.json`, `grants.json`, `flow-http.wasm`, and `<sha256>.wasm` for
//! each component. The writer is `wamn dev edge-bundle`, and the reader is
//! `wamn-edge`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// The bundle file name inside a release bundle directory.
pub const BUNDLE_FILE_NAME: &str = "edge-release.json";
/// The component facts file name inside a release bundle directory.
pub const COMPONENTS_FILE_NAME: &str = "components.json";
/// The grants file name inside a release bundle directory.
pub const GRANTS_FILE_NAME: &str = "grants.json";
/// The ingress guest file name inside a release bundle directory.
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

/// The `sha256:<hex>` digest of exact file bytes.
pub fn file_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity("sha256:".len() + digest.len() * 2);
    output.push_str("sha256:");
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a string is infallible");
    }
    output
}
