//! Per-project credential emission — the artifact 2.2b (`K8sSecretProvider`,
//! wamn-5x0.1) will consume.
//!
//! 2.3 **emits** the credential; the live in-cluster read stays 5x0.1. Two
//! shapes, both pure JSON:
//!
//! * [`projects_file`] / [`projects_file_entry`] — the `WAMN_PG_PROJECTS_FILE`
//!   format the plugin's `StaticCredentialProvider` already parses (`{ project:
//!   { "url": … } }`), so a provisioned project resolves through the exact code
//!   path production uses.
//! * [`render_secret_manifest`] — a Kubernetes `Secret` manifest (rendered as
//!   JSON, which `kubectl apply -f` accepts), named `wamn-db-<project>` — the
//!   lookup key 5x0.1 reads. `stringData.url` is the app-role connection URL.

use serde_json::{Value, json};
use wamn_registry::Triple;

use crate::name::{
    APP_ROLE, cdc_object_name, project_env_cdc_secret_name, project_env_secret_name, secret_name,
};

/// The `WAMN_PG_PROJECTS_FILE` entry for one project: `{ "url": <url> }`.
/// Policy knobs (`row_limit`, timeouts) are optional and default from the
/// plugin's base config, so the MVP entry carries only the URL.
pub fn projects_file_entry(url: &str) -> Value {
    json!({ "url": url })
}

/// A complete single-project `WAMN_PG_PROJECTS_FILE` object: `{ <project>: {
/// "url": <url> } }`.
pub fn projects_file(project: &str, url: &str) -> Value {
    json!({ project: projects_file_entry(url) })
}

/// Render the per-project credential `Secret` as a JSON manifest. Name
/// `wamn-db-<project>`; `stringData` carries the app-role URL (and the project
/// id + role for readability). `kubectl apply -f` accepts JSON, so the
/// provisioning Job can pipe this straight to the API server without a Rust K8s
/// client (that write path is deliberately kept out of 2.3 — see the crate docs).
pub fn render_secret_manifest(project: &str, namespace: &str, url: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": secret_name(project),
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-db-credentials",
                "wamn.project": project,
            },
        },
        "type": "Opaque",
        "stringData": {
            "url": url,
            "project": project,
            "role": APP_ROLE,
        },
    })
}

/// Render the per-project-env credential `Secret` (wamn-q3n.7). Name
/// `wamn-db-<org>--<project>--<env>` — the 5x0.1 lookup key recorded as the
/// project-env's `SecretRef` in the registry. `stringData.url` is the app-role
/// connection URL to the project-env database; the labels carry the full identity
/// triple so tooling never parses the name.
pub fn render_project_env_secret_manifest(triple: &Triple, namespace: &str, url: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": project_env_secret_name(&triple.org, &triple.project, triple.env.as_str()),
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-env-db-credentials",
                "wamn.org": triple.org,
                "wamn.project": triple.project,
                "wamn.env": triple.env.as_str(),
            },
        },
        "type": "Opaque",
        "stringData": {
            "url": url,
            "org": triple.org,
            "project": triple.project,
            "env": triple.env.as_str(),
            "role": APP_ROLE,
        },
    })
}

/// Render the per-project-env **CDC** credential `Secret` (wamn-l5i9.9). Name
/// `wamn-cdc-<org>--<project>--<env>` — the reference the reader registration
/// records as `replication_secret_name`, DISTINCT from the `wamn-db-…` query
/// Secret (the replication credential is its own R8b tier). `stringData.url` is
/// the replication-role connection URL to the project-env database (a plain
/// libpq URL; the reader appends its own connection parameters, e.g.
/// `replication=database`, when it opens the walsender session — l5i9.10).
pub fn render_project_env_cdc_secret_manifest(
    triple: &Triple,
    namespace: &str,
    url: &str,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": project_env_cdc_secret_name(&triple.org, &triple.project, triple.env.as_str()),
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-env-cdc-credentials",
                "wamn.org": triple.org,
                "wamn.project": triple.project,
                "wamn.env": triple.env.as_str(),
            },
        },
        "type": "Opaque",
        "stringData": {
            "url": url,
            "org": triple.org,
            "project": triple.project,
            "env": triple.env.as_str(),
            "role": cdc_object_name(&triple.org, &triple.project, triple.env.as_str()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "postgres://wamn_app:wamn_app@wamn-pg-rw:5432/wamn-db-acme";

    #[test]
    fn projects_file_matches_the_plugin_parse_shape() {
        // `StaticCredentialProvider::projects_from_json` expects
        // `{ "<project>": { "url": "…" } }`.
        let pf = projects_file("acme", URL);
        assert_eq!(pf["acme"]["url"], URL);
        assert_eq!(projects_file_entry(URL)["url"], URL);
    }

    #[test]
    fn secret_manifest_has_the_layout_5x0_1_reads() {
        let s = render_secret_manifest("acme", "wamn-system", URL);
        assert_eq!(s["kind"], "Secret");
        assert_eq!(s["metadata"]["name"], "wamn-db-acme");
        assert_eq!(s["metadata"]["namespace"], "wamn-system");
        assert_eq!(s["metadata"]["labels"]["wamn.project"], "acme");
        assert_eq!(s["type"], "Opaque");
        assert_eq!(s["stringData"]["url"], URL);
        assert_eq!(s["stringData"]["project"], "acme");
        assert_eq!(s["stringData"]["role"], "wamn_app");
    }

    #[test]
    fn project_env_secret_names_and_labels_carry_the_triple() {
        let t = Triple::new("acme", "billing", "dev");
        let url = "postgres://wamn_app:wamn_app@acme-dev-rw:5432/wamn-db-acme--billing--dev";
        let s = render_project_env_secret_manifest(&t, "wamn-system", url);
        assert_eq!(s["kind"], "Secret");
        assert_eq!(s["metadata"]["name"], "wamn-db-acme--billing--dev");
        assert_eq!(s["metadata"]["namespace"], "wamn-system");
        assert_eq!(s["metadata"]["labels"]["wamn.org"], "acme");
        assert_eq!(s["metadata"]["labels"]["wamn.project"], "billing");
        assert_eq!(s["metadata"]["labels"]["wamn.env"], "dev");
        assert_eq!(s["stringData"]["url"], url);
        assert_eq!(s["stringData"]["org"], "acme");
        assert_eq!(s["stringData"]["project"], "billing");
        assert_eq!(s["stringData"]["env"], "dev");
        assert_eq!(s["stringData"]["role"], "wamn_app");
    }

    #[test]
    fn cdc_secret_is_a_distinct_replication_tier_reference() {
        let t = Triple::new("acme", "billing", "dev");
        let url =
            "postgres://wamn_cdc_acme__billing__dev:pw@acme-dev-rw:5432/wamn-db-acme--billing--dev";
        let s = render_project_env_cdc_secret_manifest(&t, "wamn-system", url);
        assert_eq!(s["kind"], "Secret");
        // The CDC Secret name is the wamn-cdc-… sibling — NEVER the wamn-db-…
        // query Secret (a distinct R8b credential tier, one lookup key each).
        assert_eq!(s["metadata"]["name"], "wamn-cdc-acme--billing--dev");
        assert_ne!(
            s["metadata"]["name"],
            render_project_env_secret_manifest(&t, "wamn-system", url)["metadata"]["name"]
        );
        assert_eq!(
            s["metadata"]["labels"]["app.kubernetes.io/component"],
            "project-env-cdc-credentials"
        );
        assert_eq!(s["metadata"]["labels"]["wamn.org"], "acme");
        assert_eq!(s["metadata"]["labels"]["wamn.env"], "dev");
        assert_eq!(s["stringData"]["url"], url);
        // The role recorded is the underscored replication role, not wamn_app.
        assert_eq!(s["stringData"]["role"], "wamn_cdc_acme__billing__dev");
    }
}
