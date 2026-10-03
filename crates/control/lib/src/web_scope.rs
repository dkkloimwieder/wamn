//! The `config.json` a web client reads beside `index.html` (wamn-l2fi).
//!
//! The built files carry no org or project, so one build serves every
//! deployment. `wamn web upload` and the `upload-ui` step write this file at
//! the release path, and the shell reads it before sign-in.

/// The object name, beside `index.html` at the release path.
pub const SCOPE_FILE: &str = "config.json";

/// The file names the current deployment, so a browser asks again at every load.
pub const SCOPE_CACHE: &str = "no-cache";

/// The content type of the file.
pub const SCOPE_CONTENT_TYPE: &str = "application/json";

/// The bytes of the file: `{"org":"<org>","project":"<project>"}`.
pub fn scope_file_bytes(org: &str, project: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"org": org, "project": project}))
        .expect("a JSON object of two strings serializes")
}

#[cfg(test)]
mod tests {
    use super::scope_file_bytes;

    #[test]
    fn the_file_names_the_org_and_the_project() {
        assert_eq!(
            scope_file_bytes("acme", "receiving"),
            br#"{"org":"acme","project":"receiving"}"#
        );
    }
}
