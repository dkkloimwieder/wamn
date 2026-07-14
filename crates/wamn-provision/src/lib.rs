//! wamn project provisioning (2.3) — the **pure** core.
//!
//! Standing up a project turns the SQL-emitting E3 crates into a live system:
//! given a project id, provision a per-project Postgres **database** on the
//! shared cluster (D6: CloudNativePG, one shared Cluster with a database per
//! project), granting the shared [`APP_ROLE`] `CONNECT` and revoking it from
//! `PUBLIC`. The output 2.4 (system schema) consumes is *a provisioned,
//! credentialed, `wamn_app`-roled, empty project database*.
//!
//! This crate is the pure core (SR3 / house rule 1): identifier naming, the
//! `CREATE DATABASE` / role-bootstrap / `GRANT CONNECT` text builders, the
//! per-project credential Secret renderer, the connection-URL composer, and —
//! for the four-tier topology (wamn-q3n.6) — the org [`Cluster`
//! PAIR](crate::org) renderer (`<org>-prod` HA + `<org>-dev` hibernation-managed)
//! — no DB, no K8s client, no clock. The effects live in the `provision-project`
//! / `provision-org` subcommands (`wamn-host`); the `provisionbench` gate
//! (`wamn-gates`) drives the whole path against a real cluster.
//!
//! # Isolation model
//!
//! Postgres roles are **cluster-global**, so one shared cluster has one shared
//! `wamn_app` role (the grantee every generated floor and hand-written schema
//! already targets). Cross-project isolation is therefore **not** at the role
//! level — it is:
//!
//! 1. **per-project DATABASE** — a component resolved to project *a* holds a
//!    connection pool to *a*'s database only and physically cannot address
//!    another project's database (Postgres has no cross-database queries);
//! 2. **per-DB CONNECT** — `PUBLIC` is revoked and only `wamn_app` is granted
//!    `CONNECT`, so no unexpected role reaches a project database;
//! 3. **RLS within** — the 3.2 tenant floor confines rows by `app.tenant`.
//!
//! Per-project **distinct** roles/passwords (stronger credential isolation) are
//! a hardening follow-up (8.2), not this MVP.

mod error;
mod name;
pub mod org;
pub mod secret;
pub mod sql;

pub use error::ProvisionError;
pub use name::{
    APP_ROLE, DB_PREFIX, MAX_PROJECT_ID_LEN, compose_url, database_name, secret_name,
    validate_project_id,
};
pub use org::{prod_instances, render_org_cluster_pair};
