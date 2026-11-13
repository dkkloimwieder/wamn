//! The `config.json` a web client reads beside `index.html` (wamn-l2fi).
//!
//! The built files carry no org or project, so one build serves every
//! deployment. `wamn web upload` and the `upload-ui` step write this file at
//! the release path, and the shell reads it before sign-in.

/// The object name, beside `index.html` at the release path.
pub const CONFIG_FILE: &str = "config.json";

/// The file names the current deployment, so a browser asks again at every load.
pub const CONFIG_CACHE: &str = "no-cache";

/// The content type of the file.
pub const CONFIG_CONTENT_TYPE: &str = "application/json";

/// The bytes of the file: `{"org":"<org>","project":"<project>"}`.
pub fn config_file_bytes(org: &str, project: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"org": org, "project": project}))
        .expect("a JSON object of two strings serializes")
}

/// The platform UI signs in to an org without a project filter.
pub fn platform_config_file_bytes(org: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"org": org}))
        .expect("a JSON object of one string serializes")
}
