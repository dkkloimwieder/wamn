//! Rendering a project-env's CNPG `Database` custom resource (wamn-q3n.7).
//!
//! `provision-project-env` creates one per-project-env Postgres database
//! **declaratively** via CNPG's `Database` CRD (adopted in wamn-q3n.6's
//! provisioning rework): the operator reconciles the CR into a `CREATE DATABASE
//! … OWNER …` on the target cluster. The imperative work the CRD does *not* cover
//! — ensuring the `wamn_db_owner` title role (which the CR's `spec.owner` names,
//! so it must exist first) and the shared `wamn_app` role, then `ALTER DATABASE
//! … OWNER TO` / `REVOKE CONNECT` (the [`crate::sql`] builders) — stays a thin
//! privilege step (topology fact 3).
//!
//! Rendered as a `serde_json::Value` (`kubectl apply -f` accepts JSON — the
//! [`render_project_env_secret_manifest`](crate::secret::render_project_env_secret_manifest) /
//! [`render_org_cluster_set`](crate::org::render_org_cluster_set) precedent);
//! the `provision-project-env` driver emits it and the runbook/Job applies it and
//! waits ready. This crate is pure — no K8s client.

use serde_json::{Value, json};
use wamn_registry::Triple;

use crate::name::{DB_OWNER_ROLE, project_env_database_name};

/// The CNPG API group/version the `Database` CRD lives under.
const API_VERSION: &str = "postgresql.cnpg.io/v1";

/// Render the CNPG `Database` CR for a project-env database.
///
/// * `triple` — the `(org, project, env)` identity stem.
/// * `instance` — the stored `registry.environment_projects.instance_suffix`. Together
///   with `triple`, it makes the database and K8s resource name
///   `wamn-db-<org>--<project>--<env>--<instance>`
///   ([`project_env_database_name`]).
/// * `cluster` — the target CNPG `Cluster` name, chosen by the caller from the
///   org's placement via [`cluster_of`](crate::cluster_of) (D18): a
///   dedicated org's `<org>-<owner(env)>`, or the shared pool for a pooled org.
/// * `namespace` — the namespace of the CNPG `Cluster`, which the `Database`
///   must share (`--cluster-namespace`).
/// * `connection_limit` — the per-project-env `CONNECTION LIMIT`
///   (noisy-neighbour governance *within* a cluster); `None` ⇒ no limit (`-1`).
///
/// The database is **owned by [`DB_OWNER_ROLE`]** — `wamn_db_owner`, a NOLOGIN
/// title holder with zero grants and zero memberships (R9). It is deliberately
/// neither `wamn_app` nor `postgres`: guest-authored SQL executes as `wamn_app`,
/// and database ownership confers authority no grant matrix expresses and no
/// `REVOKE` can take back (`ALTER DATABASE`, `DROP DATABASE`, `CREATE SCHEMA`,
/// `GRANT … ON DATABASE`, `TEMPORARY`) — `NOCREATEDB` restrains none of it on an
/// already-owned database. `postgres` would only trade a guest-reachable owner
/// for a superuser one.
///
/// CNPG maps `spec.owner` to both `CREATE DATABASE … OWNER` and `ALTER DATABASE
/// … OWNER TO`, so this field is also the migration for databases that already
/// exist — the reconciler converges them, and
/// [`set_database_owner_sql`](crate::sql::set_database_owner_sql) is the same
/// statement for the off-cluster path. **The role must exist before any CR
/// naming it is applied.**
///
/// Catalog-publish does the schema DDL as superuser and applies the per-table
/// `FORCE ROW LEVEL SECURITY` minimum there (2.4/2.5) — wamn-q3n.7 establishes
/// only the RLS-**enforceable** substrate (a `NOBYPASSRLS`, non-guest-reachable
/// owner + per-DB `CONNECT` confinement); there are no tables to protect at
/// provision time.
///
/// `spec.ensure: present` (additive). `databaseReclaimPolicy: retain` so deleting
/// the CR never drops the underlying project database (the shared-cluster
/// guardrail — teardown drops explicitly, never as a side effect of CR deletion).
pub fn render_project_env_database(
    triple: &Triple,
    instance: &str,
    cluster: &str,
    namespace: &str,
    connection_limit: Option<i64>,
) -> Value {
    let name =
        project_env_database_name(&triple.org, &triple.project, triple.env.as_str(), instance);
    let mut spec = json!({
        "name": name,
        "owner": DB_OWNER_ROLE,
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
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-env-database",
                "wamn.org": triple.org,
                "wamn.project": triple.project,
                "wamn.env": triple.env.as_str(),
            },
            // The full coordinate, as every object of the environment carries
            // it (docs/architecture/naming.md#environment-names).
            "annotations": {
                "wamn.environment":
                    wamn_registry::name::host_status(&triple.org,&triple.project,triple.env.as_str()),
            },
        },
        "spec": spec,
    })
}

/// Render the CNPG `Publication` CR of a project-env's CDC capture.
///
/// The provisioner is not a superuser, and `CREATE PUBLICATION … FOR TABLES IN
/// SCHEMA` needs one, so the CNPG operator creates the publication
/// (`docs/plan/platform-ui.md` §4.10). It is the publication that
/// [`create_publication_sql`](crate::sql::create_publication_sql) declares:
/// `publication` over every table of `schemas`, with
/// [`CDC_PUBLISH`](crate::sql::CDC_PUBLISH). The resource takes the database
/// name, a valid Kubernetes name, and shares the namespace of its `Cluster`.
///
/// Apply it after the CDC SQL creates the schema, and wait for
/// `status.applied` before any write to the schema: the slot decodes a change
/// only under a publication that existed when the change was written.
pub fn render_project_env_publication(
    triple: &Triple,
    instance: &str,
    cluster: &str,
    namespace: &str,
    publication: &str,
    schemas: &[String],
) -> Value {
    let database =
        project_env_database_name(&triple.org, &triple.project, triple.env.as_str(), instance);
    json!({
        "apiVersion": API_VERSION,
        "kind": "Publication",
        "metadata": {
            "name": database,
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-env-publication",
                "wamn.org": triple.org,
                "wamn.project": triple.project,
                "wamn.env": triple.env.as_str(),
            },
        },
        "spec": {
            "cluster": { "name": cluster },
            "dbname": database,
            "name": publication,
            "target": { "objects": schemas.iter().map(|schema| json!({"tablesInSchema": schema})).collect::<Vec<_>>() },
            "parameters": { "publish": crate::sql::CDC_PUBLISH },
            "publicationReclaimPolicy": "retain",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reclaim_policy_retains_the_database_on_cr_deletion() {
        // Deleting the CR must NOT drop the project database (shared-cluster
        // guardrail): the reclaim policy is `retain`, never `delete`.
        let t = Triple::new("acme", "prod", "billing");
        let cr = render_project_env_database(&t, "k3m9x2p7", "acme-prod", "wamn-system", None);
        assert_eq!(cr["spec"]["databaseReclaimPolicy"], "retain");
    }

    #[test]
    fn connection_limit_is_omitted_by_default_and_set_when_given() {
        let t = Triple::new("acme", "prod", "billing");
        // Default: no CONNECTION LIMIT (the field is absent → operator uses -1).
        let cr = render_project_env_database(&t, "k3m9x2p7", "acme-prod", "wamn-system", None);
        assert!(cr["spec"]["connectionLimit"].is_null());
        // Set: per-project-env noisy-neighbour cap.
        let cr = render_project_env_database(&t, "k3m9x2p7", "acme-prod", "wamn-system", Some(20));
        assert_eq!(cr["spec"]["connectionLimit"], 20);
    }

    #[test]
    fn env_selects_the_cluster_the_caller_passes() {
        // The renderer does not decide the cluster — the caller picks it by
        // env→side. canary and prod carry distinct db names but the SAME (prod)
        // cluster; dev carries the dev cluster.
        let cr_prod = render_project_env_database(
            &Triple::new("acme", "prod", "billing"),
            "k3m9x2p7",
            "acme-prod",
            "wamn-system",
            None,
        );
        let cr_canary = render_project_env_database(
            &Triple::new("acme", "canary", "billing"),
            "k3m9x2p7",
            "acme-prod",
            "wamn-system",
            None,
        );
        let cr_dev = render_project_env_database(
            &Triple::new("acme", "dev", "billing"),
            "k3m9x2p7",
            "acme-dev",
            "wamn-system",
            None,
        );
        assert_eq!(cr_prod["spec"]["cluster"]["name"], "acme-prod");
        assert_eq!(cr_canary["spec"]["cluster"]["name"], "acme-prod");
        assert_eq!(cr_dev["spec"]["cluster"]["name"], "acme-dev");
        assert_ne!(cr_prod["spec"]["name"], cr_canary["spec"]["name"]);
    }
}
