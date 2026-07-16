//! Rendering a project-env's CNPG `Database` custom resource (wamn-q3n.7).
//!
//! `provision-project-env` creates one per-project-env Postgres database
//! **declaratively** via CNPG's `Database` CRD (adopted in wamn-q3n.6's
//! provisioning rework): the operator reconciles the CR into a `CREATE DATABASE
//! … OWNER …` on the target cluster. The imperative work the CRD does *not* cover
//! — ensuring the shared `wamn_app` role and `REVOKE CONNECT FROM PUBLIC` /
//! `GRANT` (the [`crate::sql`] builders) — stays a thin privilege step (topology
//! fact 3).
//!
//! Rendered as a `serde_json::Value` (`kubectl apply -f` accepts JSON — the
//! [`render_secret_manifest`](crate::secret::render_secret_manifest) /
//! [`render_org_cluster_set`](crate::org::render_org_cluster_set) precedent);
//! the `provision-project-env` driver emits it and the runbook/Job applies it and
//! waits ready. This crate is pure — no K8s client.

use serde_json::{Value, json};
use wamn_registry::Triple;

use crate::name::{APP_ROLE, project_env_database_name};

/// The CNPG API group/version the `Database` CRD lives under.
const API_VERSION: &str = "postgresql.cnpg.io/v1";
/// The namespace project-env `Database` resources live in (alongside the clusters).
const NAMESPACE: &str = "wamn-system";

/// Render the CNPG `Database` CR for a project-env database.
///
/// * `triple` — the `(org, project, env)` identity. The database and K8s resource
///   name is `wamn-db-<org>--<project>--<env>` ([`project_env_database_name`]).
/// * `cluster` — the target CNPG `Cluster` name, chosen by the caller from the
///   org's placement via [`cluster_of`](wamn_registry::cluster_of) (D18): a
///   dedicated org's `<org>-<owner(env)>`, or the shared pool for a pooled org.
/// * `connection_limit` — the per-project-env `CONNECTION LIMIT`
///   (noisy-neighbour governance *within* a cluster); `None` ⇒ no limit (`-1`).
///
/// The database is **owned by the shared least-privilege [`APP_ROLE`]**: `wamn_app`
/// is `NOSUPERUSER NOCREATEDB NOBYPASSRLS`, so no tenant database is
/// superuser-owned. Catalog-publish does the schema DDL as superuser and applies
/// the per-table `FORCE ROW LEVEL SECURITY` floor there (2.4/2.5) — wamn-q3n.7
/// establishes only the RLS-**enforceable** substrate (a `NOBYPASSRLS` owner +
/// per-DB `CONNECT` confinement); there are no tables to protect at provision time.
///
/// `spec.ensure: present` (additive). `databaseReclaimPolicy: retain` so deleting
/// the CR never drops the underlying tenant database (the shared-cluster
/// guardrail — teardown drops explicitly, never as a side effect of CR deletion).
pub fn render_project_env_database(
    triple: &Triple,
    cluster: &str,
    connection_limit: Option<i64>,
) -> Value {
    let name = project_env_database_name(&triple.org, &triple.project, triple.env.as_str());
    let mut spec = json!({
        "name": name,
        "owner": APP_ROLE,
        "cluster": { "name": cluster },
        "ensure": "present",
        "databaseReclaimPolicy": "retain",
    });
    if let Some(limit) = connection_limit {
        spec["connectionLimit"] = json!(limit);
    }
    json!({
        "apiVersion": API_VERSION,
        "kind": "Database",
        "metadata": {
            "name": name,
            "namespace": NAMESPACE,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-env-database",
                "wamn.org": triple.org,
                "wamn.project": triple.project,
                "wamn.env": triple.env.as_str(),
            },
        },
        "spec": spec,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_cr_names_the_db_owner_and_target_cluster() {
        let t = Triple::new("acme", "billing", "dev");
        let cr = render_project_env_database(&t, "acme-dev", None);
        assert_eq!(cr["apiVersion"], "postgresql.cnpg.io/v1");
        assert_eq!(cr["kind"], "Database");
        // Resource name == PG database name == wamn-db-<org>--<project>--<env>.
        assert_eq!(cr["metadata"]["name"], "wamn-db-acme--billing--dev");
        assert_eq!(cr["spec"]["name"], "wamn-db-acme--billing--dev");
        assert_eq!(cr["metadata"]["namespace"], "wamn-system");
        // Owned by the shared least-privilege role; created on the target cluster.
        assert_eq!(cr["spec"]["owner"], "wamn_app");
        assert_eq!(cr["spec"]["cluster"]["name"], "acme-dev");
        assert_eq!(cr["spec"]["ensure"], "present");
        // Identity labels so tooling never parses the name.
        assert_eq!(cr["metadata"]["labels"]["wamn.org"], "acme");
        assert_eq!(cr["metadata"]["labels"]["wamn.project"], "billing");
        assert_eq!(cr["metadata"]["labels"]["wamn.env"], "dev");
        assert_eq!(
            cr["metadata"]["labels"]["app.kubernetes.io/managed-by"],
            "wamn"
        );
    }

    #[test]
    fn reclaim_policy_retains_the_database_on_cr_deletion() {
        // Deleting the CR must NOT drop the tenant database (shared-cluster
        // guardrail): the reclaim policy is `retain`, never `delete`.
        let t = Triple::new("acme", "billing", "prod");
        let cr = render_project_env_database(&t, "acme-prod", None);
        assert_eq!(cr["spec"]["databaseReclaimPolicy"], "retain");
    }

    #[test]
    fn connection_limit_is_omitted_by_default_and_set_when_given() {
        let t = Triple::new("acme", "billing", "prod");
        // Default: no CONNECTION LIMIT (the field is absent → operator uses -1).
        let cr = render_project_env_database(&t, "acme-prod", None);
        assert!(cr["spec"]["connectionLimit"].is_null());
        // Set: per-project-env noisy-neighbour cap.
        let cr = render_project_env_database(&t, "acme-prod", Some(20));
        assert_eq!(cr["spec"]["connectionLimit"], 20);
    }

    #[test]
    fn env_selects_the_cluster_the_caller_passes() {
        // The renderer does not decide the cluster — the caller picks it by
        // env→side. canary and prod carry distinct db names but the SAME (prod)
        // cluster; dev carries the dev cluster.
        let cr_prod =
            render_project_env_database(&Triple::new("acme", "billing", "prod"), "acme-prod", None);
        let cr_canary = render_project_env_database(
            &Triple::new("acme", "billing", "canary"),
            "acme-prod",
            None,
        );
        let cr_dev =
            render_project_env_database(&Triple::new("acme", "billing", "dev"), "acme-dev", None);
        assert_eq!(cr_prod["spec"]["cluster"]["name"], "acme-prod");
        assert_eq!(cr_canary["spec"]["cluster"]["name"], "acme-prod");
        assert_eq!(cr_dev["spec"]["cluster"]["name"], "acme-dev");
        assert_ne!(cr_prod["spec"]["name"], cr_canary["spec"]["name"]);
    }
}
