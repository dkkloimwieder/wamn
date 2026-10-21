//! Host-owned environment credential store.
//!
//! Portable effects resolve a credential handle selected by the admitted
//! connection generation. Secret material never enters the guest or run facts.
//!
//! - Ordinary handles use the host-owned project identity. Org handles use
//!   the trusted host org and never fall back to project credentials.
//! - `WAMN_CREDENTIALS_FILE` is a mounted static file. Version 2 carries
//!   separate `projects` and `orgs` maps. Legacy project maps still parse.
//! - No configured source is `unavailable`; a configured source missing the
//!   selected handle is `not-found`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialError {
    NotFound,
    Unavailable,
}

/// Project and org credentials loaded from the fixed Secret mount.
pub struct WamnCredentials {
    /// Whether ANY backing source was configured. Distinguishes `unavailable`
    /// (no source — retryable) from `not-found` (source present, name absent —
    /// config-shaped).
    has_source: bool,
    /// project → { credential name → secret material }.
    projects: HashMap<String, HashMap<String, String>>,
    orgs: HashMap<String, HashMap<String, String>>,
    org: Option<String>,
}

impl std::fmt::Debug for WamnCredentials {
    /// Names the shape only. The map holds secret material, so no key or value
    /// of it reaches a log.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WamnCredentials")
            .field("has_source", &self.has_source)
            .field("projects", &self.projects.len())
            .finish_non_exhaustive()
    }
}

impl WamnCredentials {
    /// A vault with NO backing source: every lookup is unavailable.
    /// Gates and credential-less deployments use this.
    pub fn empty() -> Self {
        Self {
            has_source: false,
            projects: HashMap::new(),
            orgs: HashMap::new(),
            org: None,
        }
    }

    /// A vault over an explicit per-project map (tests / the gate harness).
    pub fn from_projects(projects: HashMap<String, HashMap<String, String>>) -> Self {
        Self {
            has_source: true,
            projects,
            orgs: HashMap::new(),
            org: None,
        }
    }

    /// Parse the mounted credentials file: a JSON object
    /// `{ "<project>": { "<name>": "<secret>", ... }, ... }`, mounted from a
    /// K8s Secret.
    pub fn projects_from_json(
        text: &str,
    ) -> anyhow::Result<HashMap<String, HashMap<String, String>>> {
        let root: serde_json::Value =
            serde_json::from_str(text).context("credentials file is not valid JSON")?;
        let obj = root
            .as_object()
            .context("credentials file must be a JSON object of projects")?;
        let mut projects = HashMap::new();
        for (project, creds) in obj {
            let creds_obj = creds.as_object().with_context(|| {
                format!("credentials for project {project:?} must be an object of name: secret")
            })?;
            let mut map = HashMap::new();
            for (name, secret) in creds_obj {
                let secret = secret.as_str().with_context(|| {
                    format!("credential {project:?}/{name:?} must be a string secret")
                })?;
                map.insert(name.clone(), secret.to_string());
            }
            projects.insert(project.clone(), map);
        }
        Ok(projects)
    }

    /// Parse either the legacy project map or the version 2 credentials envelope.
    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            version: u32,
            projects: HashMap<String, HashMap<String, String>>,
            orgs: HashMap<String, HashMap<String, String>>,
        }
        let root: serde_json::Value =
            serde_json::from_str(text).context("credentials file is not valid JSON")?;
        if !root
            .get("version")
            .is_some_and(serde_json::Value::is_number)
        {
            return Ok(Self::from_projects(Self::projects_from_json(text)?));
        }
        let envelope: Envelope =
            serde_json::from_value(root).context("invalid version 2 credentials file")?;
        anyhow::ensure!(
            envelope.version == 2,
            "unsupported credentials file version"
        );
        Ok(Self {
            has_source: true,
            projects: envelope.projects,
            orgs: envelope.orgs,
            org: None,
        })
    }

    /// Bind the trusted host org before sharing this process-lifetime snapshot.
    pub fn with_org(mut self, org: &str) -> Self {
        self.org = Some(org.to_owned());
        self
    }

    /// Resolve an org handle only within the host org, without project fallback.
    pub fn lookup_connection(&self, project: &str, handle: &str) -> Option<String> {
        if handle.starts_with("org:") {
            let (org, name) = wamn_catalog::parse_org_credential_handle(handle).ok()?;
            if self.org.as_deref() != Some(org) {
                return None;
            }
            return self.orgs.get(org)?.get(name).cloned();
        }
        self.lookup(project, handle)
    }

    /// A vault from the file at `path`. A MISSING file is a warn + empty
    /// vault (the deploy manifest mounts the Secret `optional`, so a
    /// credential-less project deploys cleanly); a present-but-malformed file
    /// is a hard error (a real misconfiguration must be loud).
    ///
    /// # Rotation is a pod roll, not a reload — decided once, recorded here
    ///
    /// This reads once, at startup, so material rotated under an UNCHANGED
    /// handle is not observed until the process restarts. An unchanged handle is
    /// exactly what a rotation keeps: a rotation mints a new credential
    /// GENERATION and leaves the portable artifact — and therefore the
    /// connection definition that names the handle — untouched
    /// (`docs/archive/data-path/credential-vault.md`). Snapshotting is
    /// nonetheless the correct posture, not an omission, because an attempt's
    /// durable facts pin the `credential_generation` that authorized it
    /// (`wamn_run.effect_attempts`), which recovery must reuse or explicitly
    /// refuse. This file's `{project: {handle: secret}}` shape carries no
    /// generation, so a watcher could not tell rotated material from the pinned
    /// material and would silently substitute credentials under a recovering
    /// attempt. A process-lifetime snapshot cannot.
    ///
    /// Supporting rotation therefore means making the generation part of the
    /// source, not adding a reload here. Until then the rotation procedure is a
    /// roll: `deploy/mvp/bootstrap.sh` already rolls the runner Deployment after
    /// it publishes replacement credential material.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            tracing::warn!(
                path = %path.display(),
                "credentials file not found — the vault is empty (every get is unavailable)"
            );
            return Ok(Self::empty());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read credentials file {}", path.display()))?;
        Self::from_json(&text)
    }

    /// Resolve a `(project, name)` secret for the native effect adapter.
    /// `None` if the name is absent or no source is configured.
    pub fn lookup(&self, project: &str, name: &str) -> Option<String> {
        self.resolve(project, name).ok()
    }

    /// Resolve `name` within `project`.
    fn resolve(&self, project: &str, name: &str) -> Result<String, CredentialError> {
        if !self.has_source {
            return Err(CredentialError::Unavailable);
        }
        self.projects
            .get(project)
            .and_then(|creds| creds.get(name))
            .cloned()
            .ok_or(CredentialError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_project_vault() -> WamnCredentials {
        WamnCredentials::from_projects(HashMap::from([(
            "proj-a".to_string(),
            HashMap::from([("notify-token".to_string(), "s3cr3t".to_string())]),
        )]))
    }

    /// The unavailable/not-found split: no source at all is retryable
    /// infrastructure; a present source lacking the name is config-shaped.
    #[test]
    fn resolve_distinguishes_no_source_from_unknown_name() {
        let empty = WamnCredentials::empty();
        assert!(matches!(
            empty.resolve("proj-a", "notify-token"),
            Err(CredentialError::Unavailable)
        ));
        let vault = one_project_vault();
        assert_eq!(vault.resolve("proj-a", "notify-token").unwrap(), "s3cr3t");
        assert!(matches!(
            vault.resolve("proj-a", "unknown"),
            Err(CredentialError::NotFound)
        ));
    }

    /// PROJECT SCOPING is the host-enforced boundary: the same name in a
    /// different project resolves that project's secret (or nothing) — a
    /// component can never read across projects.
    #[test]
    fn resolution_is_project_scoped() {
        let vault = WamnCredentials::from_projects(HashMap::from([
            (
                "proj-a".to_string(),
                HashMap::from([("token".to_string(), "secret-a".to_string())]),
            ),
            (
                "proj-b".to_string(),
                HashMap::from([("token".to_string(), "secret-b".to_string())]),
            ),
        ]));
        assert_eq!(vault.resolve("proj-a", "token").unwrap(), "secret-a");
        assert_eq!(vault.resolve("proj-b", "token").unwrap(), "secret-b");
        assert!(matches!(
            vault.resolve("proj-c", "token"),
            Err(CredentialError::NotFound)
        ));
    }

    /// The mounted-file shape is a project-keyed JSON object:
    /// `{project: {name: secret}}`, strings only, malformed = loud.
    #[test]
    fn credentials_file_parses_the_nested_project_shape() {
        let projects = WamnCredentials::projects_from_json(
            r#"{"default": {"notify-token": "tok-1", "other": "tok-2"}, "p2": {}}"#,
        )
        .unwrap();
        assert_eq!(projects["default"]["notify-token"], "tok-1");
        assert_eq!(projects["default"]["other"], "tok-2");
        assert!(projects["p2"].is_empty());

        assert!(WamnCredentials::projects_from_json("[]").is_err());
        assert!(WamnCredentials::projects_from_json(r#"{"p": "flat"}"#).is_err());
        assert!(WamnCredentials::projects_from_json(r#"{"p": {"n": 7}}"#).is_err());
    }

    /// Material is a PROCESS-LIFETIME snapshot: rotating the mount under the
    /// same handle is not observed. A later reload would resolve material no
    /// pinned `credential_generation` names, so it has to fail here rather than
    /// land as a silent behaviour change.
    #[test]
    fn from_file_snapshots_material_for_the_process_lifetime() {
        let path = std::env::temp_dir().join(format!(
            "wamn-credentials-snapshot-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, r#"{"proj-a": {"token": "generation-1"}}"#)
            .expect("write the mounted credentials file");
        let vault = WamnCredentials::from_file(&path).expect("snapshot the mount");
        std::fs::write(&path, r#"{"proj-a": {"token": "generation-2"}}"#)
            .expect("rotate the material under the same handle");
        let resolved = vault.lookup("proj-a", "token");
        std::fs::remove_file(&path).expect("remove the test mount");
        assert_eq!(resolved.as_deref(), Some("generation-1"));
    }
}

#[cfg(test)]
mod org_tests {
    use super::*;

    #[test]
    fn version_two_scopes_connections_and_keeps_route_lookup_project_only() {
        let text = r#"{"version":2,"projects":{"p":{"token":"project","org:other:erp":"fallback","org:acme:missing":"fallback"}},"orgs":{"acme":{"erp":"shared"},"other":{"erp":"other-secret"}}}"#;
        let unscoped = WamnCredentials::from_json(text).unwrap();
        assert!(unscoped.lookup_connection("p", "org:acme:erp").is_none());
        let vault = unscoped.with_org("acme");
        assert_eq!(
            vault.lookup_connection("p", "org:acme:erp").as_deref(),
            Some("shared")
        );
        assert_eq!(
            vault
                .lookup_connection("another-project", "org:acme:erp")
                .as_deref(),
            Some("shared")
        );
        assert_eq!(
            vault.lookup_connection("p", "token").as_deref(),
            Some("project")
        );
        assert!(vault.lookup_connection("p", "org:other:erp").is_none());
        assert!(vault.lookup_connection("p", "org:acme:missing").is_none());
        assert!(vault.lookup_connection("p", "org:acme:erp:extra").is_none());
        assert!(vault.lookup("p", "org:acme:erp").is_none());
    }

    #[test]
    fn invalid_envelopes_and_missing_files_have_distinct_results() {
        for text in [
            r#"{"version":3,"projects":{},"orgs":{}}"#,
            r#"{"version":2,"projects":{},"orgs":{"acme":{"erp":7}}}"#,
            r#"{"version":2,"projects":{},"orgs":{},"extra":true}"#,
        ] {
            assert!(WamnCredentials::from_json(text).is_err());
        }
        let path =
            std::env::temp_dir().join(format!("absent-org-credentials-{}", std::process::id()));
        assert!(!path.exists());
        assert!(
            WamnCredentials::from_file(&path)
                .unwrap()
                .lookup_connection("p", "org:acme:erp")
                .is_none()
        );
    }
}
