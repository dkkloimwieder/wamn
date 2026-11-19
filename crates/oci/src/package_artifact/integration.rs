//! Media types for integration artifacts.

/// OCI artifact type for one integration.
pub const ARTIFACT_TYPE: &str = "application/vnd.wamn.integration.v1";
/// OCI configuration media type for integration identity and source bytes.
pub const CONFIG_TYPE: &str = "application/vnd.wamn.integration.config.v1+json";
/// OCI layer media type for integration component bytes.
pub const WASM_TYPE: &str = "application/wasm";
