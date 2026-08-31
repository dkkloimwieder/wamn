//! Byte-coherence guard for vendored routing WIT packages.

use std::fs;
use std::path::{Path, PathBuf};

const HOST_FLOW_COPY: &str = include_str!("../wit/deps/wamn-flow-http-routing/package.wit");
const HTTP_FLOW_COPY: &str = include_str!(
    "../../../../components/ingress/http-route/wit/deps/wamn-flow-http-routing/package.wit"
);
const DELIVERY_FLOW_COPY: &str =
    include_str!("../../../execution/host/wit/deps/wamn-flow-http-routing/package.wit");
const MATERIALIZER_FLOW_COPY: &str = include_str!(
    "../../../../components/execution/materializer/wit/deps/wamn-flow-http-routing/package.wit"
);

const NATIVE_DELIVERY_COPY: &str =
    include_str!("../../../execution/host/wit/deps/wamn-router-delivery/package.wit");
const HTTP_DELIVERY_COPY: &str = include_str!(
    "../../../../components/ingress/http-route/wit/deps/wamn-router-delivery/package.wit"
);
const MATERIALIZER_DELIVERY_COPY: &str = include_str!(
    "../../../../components/execution/materializer/wit/deps/wamn-router-delivery/package.wit"
);

const REGISTERED_FLOW_COPIES: [&str; 4] = [
    "components/execution/materializer/wit/deps/wamn-flow-http-routing/package.wit",
    "components/ingress/http-route/wit/deps/wamn-flow-http-routing/package.wit",
    "crates/execution/host/wit/deps/wamn-flow-http-routing/package.wit",
    "crates/platform/runtime/wit/deps/wamn-flow-http-routing/package.wit",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root canonicalizes")
}

fn collect_flow_copies(dir: &Path, root: &Path, found: &mut Vec<String>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|error| panic!("{} reads: {error}", dir.display()));
    for entry in entries {
        let entry = entry.expect("directory entry reads");
        let path = entry.path();
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | "target")
            ) {
                continue;
            }
            collect_flow_copies(&path, root, found);
            continue;
        }
        let is_flow_copy = path.file_name().and_then(|name| name.to_str()) == Some("package.wit")
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some("wamn-flow-http-routing");
        if is_flow_copy {
            found.push(
                path.strip_prefix(root)
                    .expect("vendored WIT is within repository")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
}

#[test]
fn the_flow_http_routing_copies_stay_byte_identical() {
    assert_eq!(HOST_FLOW_COPY, HTTP_FLOW_COPY);
    assert_eq!(HOST_FLOW_COPY, DELIVERY_FLOW_COPY);
    assert_eq!(HOST_FLOW_COPY, MATERIALIZER_FLOW_COPY);
}

#[test]
fn every_flow_http_routing_copy_is_registered() {
    let root = repo_root();
    let mut found = Vec::new();
    for top in ["components", "crates", "services"] {
        collect_flow_copies(&root.join(top), &root, &mut found);
    }
    found.sort();
    assert_eq!(found, REGISTERED_FLOW_COPIES);
}

#[test]
fn the_router_delivery_copies_stay_byte_identical() {
    assert_eq!(NATIVE_DELIVERY_COPY, HTTP_DELIVERY_COPY);
    assert_eq!(NATIVE_DELIVERY_COPY, MATERIALIZER_DELIVERY_COPY);
}
