//! The shared catalog-pinning arc for the run-next gate harnesses (wamn-kex2).
//!
//! `run-next` resolves a run's graph ONLY through the immutable release lineage:
//! the flowrunner's `PINNED_ARTIFACT_SQL` joins `catalog.release_flows` ->
//! `release_manifests` -> `flow_artifacts` and admits a member only when the
//! manifest entry, the stored artifact, and the run's own
//! `invocation_context #>> '{principal,artifact-digest}'` all agree. A gate that
//! seeds only the mutable `{schema}.flows` head therefore dies twice over —
//! `42P01 catalog.release_flows` while the catalog schema is absent, and
//! `run does not resolve to exactly one immutable pinned flow artifact` once it
//! is present but empty. Every gate that reaches run-next needs the same two
//! moves, so they live here once:
//!
//!   * [`publish_release`] — the canonical catalog storage DDL, dropped and
//!     recreated, then ONE immutable release (catalog header + `flow_artifacts` +
//!     `release_manifests` + `release_flows`) committed as a unit, because the
//!     membership-coherence triggers are DEFERRABLE INITIALLY DEFERRED and hold
//!     only with the whole release present. `catalog` is a single GLOBAL schema
//!     per database (unlike the per-case ephemeral run schemas), so a leftover
//!     release from an earlier gate in the same database would mask a seeding
//!     bug: metricbench buys that hermeticity with a throwaway database, these
//!     gates buy it with the drop.
//!   * [`pin_run`] — pin a seeded run to that release and stamp the trusted
//!     invocation context admission would have written. The principal's
//!     `artifact-digest` is read FROM `catalog.flow_artifacts`, never written as a
//!     literal: `PinnedArtifact::from_storage` verifies the digest and skips an
//!     artifact that disagrees, so a placeholder pins nothing.
//!
//! Each gate keeps its own run-plane DDL, tenant, schema, and run seeding; this
//! module owns only the catalog arc.

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::Artifact;
use wamn_flow::Flow;
use wamn_node_manifest::{CapabilityClass, ResolvedNodeInterface};

/// The canonical catalog storage DDL — the same standalone deploy artifact
/// metricbench and testkitbench lay down, so no gate fixture can drift from the
/// schema of record.
const CATALOG_DDL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

/// The catalog each gate release is published under, suffixed per release. A
/// release pins at most ONE version of a flow (`catalog.release_flows` is keyed on
/// `(tenant, catalog, catalog_version, flow_id)`) and `catalog.catalogs` admits one
/// `applied` version per (catalog, environment), so a gate that publishes two
/// versions of the same flow — runnerbench's hot-reload phase — gets a second
/// catalog rather than a second version of the first.
const CATALOG_ID_PREFIX: &str = "gate-catalog-";
/// The version every gate catalog is published at.
const CATALOG_VERSION: i32 = 1;
/// The environment those releases are applied in.
const ENVIRONMENT: &str = "gate";

/// One flow artifact, published as a member of the gate's release.
struct Member {
    flow_id: String,
    flow_version: i32,
    artifact_hash: String,
    graph_json: String,
    graph_hash: String,
}

/// The capability class each gate-fixture node type is published with.
///
/// Publication resolves every node type to a capability class and a recovery
/// contract. The gates' fixtures draw from the standard node library, whose
/// classification is fixed: effectful types are never-replay (their outputs are
/// recorded and skipped on resume — the property the failover phases prove),
/// pure types replay.
fn node_capability(node_type: &str) -> anyhow::Result<CapabilityClass> {
    Ok(match node_type {
        "conditional" | "cron" | "event" | "request" | "respond" | "transform" => {
            CapabilityClass::Pure
        }
        "http-call" => CapabilityClass::Http,
        "pg-write" | "postgres-query" => CapabilityClass::Postgres,
        other => anyhow::bail!(
            "gate fixture node type {other:?} has no published interface: give it a \
             capability class here, or the release cannot be published"
        ),
    })
}

/// The implementations a graph requires: exactly one per DISTINCT node type,
/// ordered by node type (artifact identity demands both). Derived from the graph,
/// so editing a fixture cannot leave a stale interface set behind.
fn implementations(flow: &Flow) -> anyhow::Result<Vec<ResolvedNodeInterface>> {
    let mut node_types: Vec<&str> = flow
        .nodes
        .iter()
        .map(|node| node.node_type.as_str())
        .collect();
    node_types.sort_unstable();
    node_types.dedup();

    node_types
        .into_iter()
        .map(|node_type| {
            let capability = node_capability(node_type)?;
            Ok(ResolvedNodeInterface::new(
                node_type,
                "wamn:node/node@0.1.0",
                vec!["main".to_string()],
                vec![capability],
                Vec::new(),
            ))
        })
        .collect()
}

/// Build the immutable artifact for one gate fixture graph.
fn member(tenant: &str, flow_json: &str) -> anyhow::Result<Member> {
    let flow = Flow::from_json(flow_json)
        .map_err(|error| anyhow::anyhow!("parse gate fixture graph: {error}"))?;
    let flow_version = i32::try_from(flow.version).context("gate fixture flow version")?;
    let artifact = Artifact::new(tenant, &flow, implementations(&flow)?).map_err(|error| {
        anyhow::anyhow!("build the immutable {} artifact: {error}", flow.flow_id)
    })?;
    Ok(Member {
        flow_id: flow.flow_id.clone(),
        flow_version,
        artifact_hash: artifact.identity().artifact_hash().as_str().to_string(),
        graph_json: String::from_utf8(flow.canonical_bytes())
            .expect("canonical flow graph is UTF-8"),
        graph_hash: artifact.graph_hash().to_string(),
    })
}

/// Lay the canonical catalog schema down hermetically. The DDL grants to
/// `wamn_scenario_author`, a NOLOGIN authoring role the gate databases do not
/// otherwise need, so it is ensured first.
async fn apply_catalog_preamble(client: &Client) -> anyhow::Result<()> {
    client
        .batch_execute(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') \
                 THEN CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOBYPASSRLS; \
               END IF; \
             END $$;",
        )
        .await
        .context("ensure the wamn_scenario_author role")?;
    client
        .batch_execute("DROP SCHEMA IF EXISTS catalog CASCADE;")
        .await
        .context("drop any leftover catalog schema")?;
    client
        .batch_execute(CATALOG_DDL)
        .await
        .context("apply the canonical catalog storage DDL")?;
    Ok(())
}

/// Publish `flows` as immutable release members on a fresh catalog schema.
/// `admin_url` is the superuser URL each gate already provisions with.
///
/// A release pins at most one version of a flow, so the flows are grouped by
/// flow id and the Nth version of any flow lands in the Nth release. Every
/// release's four rows share a transaction: `release_flows` carries a foreign key
/// to its manifest and the membership-coherence trigger is DEFERRABLE INITIALLY
/// DEFERRED, so a release is coherent only at commit.
pub(crate) async fn publish_release(
    admin_url: &str,
    tenant: &str,
    flows: &[String],
) -> anyhow::Result<()> {
    let mut releases: Vec<Vec<Member>> = Vec::new();
    for flow_json in flows {
        let member = member(tenant, flow_json)?;
        let slot = releases
            .iter()
            .position(|release: &Vec<Member>| {
                !release.iter().any(|held| held.flow_id == member.flow_id)
            })
            .unwrap_or_else(|| {
                releases.push(Vec::new());
                releases.len() - 1
            });
        releases[slot].push(member);
    }

    let (mut client, connection) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect to publish the gate release")?;
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });
    let result = async {
        apply_catalog_preamble(&client).await?;
        for (index, members) in releases.iter().enumerate() {
            let catalog_id = format!("{CATALOG_ID_PREFIX}{}", index + 1);
            let members_json = Value::Array(
                members
                    .iter()
                    .map(|member| {
                        json!({
                            "flow-id": member.flow_id,
                            "flow-version": member.flow_version,
                            "artifact-hash": member.artifact_hash,
                        })
                    })
                    .collect(),
            );
            let transaction = client.transaction().await?;
            transaction
                .execute(
                    "INSERT INTO catalog.catalogs \
                       (tenant_id,catalog_id,version,environment,schema_version,state,document) \
                     VALUES ($1,$2,$3,$4,'0.1','applied','{}')",
                    &[&tenant, &catalog_id, &CATALOG_VERSION, &ENVIRONMENT],
                )
                .await
                .context("seed the release's catalog header")?;
            for member in members {
                transaction
                    .execute(
                        "INSERT INTO catalog.flow_artifacts \
                           (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash, \
                            artifact_hash) \
                         VALUES ($1,$2,$3,'0.1',$4::text::jsonb,$5,$6)",
                        &[
                            &tenant,
                            &member.flow_id,
                            &member.flow_version,
                            &member.graph_json,
                            &member.graph_hash,
                            &member.artifact_hash,
                        ],
                    )
                    .await
                    .with_context(|| format!("publish the {} flow artifact", member.flow_id))?;
            }
            transaction
                .execute(
                    "INSERT INTO catalog.release_manifests \
                       (tenant_id,catalog_id,catalog_version,members_json) VALUES ($1,$2,$3,$4)",
                    &[&tenant, &catalog_id, &CATALOG_VERSION, &members_json],
                )
                .await
                .context("publish the release manifest")?;
            for member in members {
                transaction
                    .execute(
                        "INSERT INTO catalog.release_flows \
                           (tenant_id,catalog_id,catalog_version,flow_id,flow_version) \
                         VALUES ($1,$2,$3,$4,$5)",
                        &[
                            &tenant,
                            &catalog_id,
                            &CATALOG_VERSION,
                            &member.flow_id,
                            &member.flow_version,
                        ],
                    )
                    .await
                    .with_context(|| format!("register {} as a release member", member.flow_id))?;
            }
            transaction
                .commit()
                .await
                .context("commit the immutable gate release")?;
            for member in members {
                println!(
                    "release {catalog_id}@{CATALOG_VERSION} pins {} v{} {}",
                    member.flow_id, member.flow_version, member.artifact_hash
                );
            }
        }
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

/// Pin a seeded run to the published release and stamp the trusted invocation
/// context admission would have written.
///
/// `client` must already be scoped to the gate's schema + tenant claim: the
/// statement is unqualified and the `runs` predicate reads `app.tenant`. The
/// release the run is pinned to and the principal's `artifact-digest` are BOTH
/// joined out of the published membership, so the pin, the release, and the
/// principal cannot disagree — a literal digest here is the mutation that turns
/// run-next back into `invalid active flow skipped`.
pub(crate) async fn pin_run(client: &Client, run_id: &str) -> anyhow::Result<()> {
    let pinned = client
        .execute(
            "UPDATE runs AS r \
                SET catalog_id = rf.catalog_id, catalog_version = rf.catalog_version, \
                    environment = $2, \
                    invocation_context = jsonb_build_object( \
                      'version', 1, \
                      'principal', jsonb_build_object( \
                        'tenant-id', r.tenant_id, 'environment', $2::text, \
                        'catalog-id', rf.catalog_id, \
                        'catalog-version', rf.catalog_version::bigint, \
                        'run-id', r.run_id, 'flow-id', r.flow_id, \
                        'flow-version', r.flow_version, \
                        'artifact-digest', a.artifact_hash), \
                      'source', jsonb_build_object('producer', r.trigger_source)) \
               FROM catalog.release_flows AS rf \
               JOIN catalog.flow_artifacts AS a \
                 ON a.tenant_id = rf.tenant_id AND a.flow_id = rf.flow_id \
                AND a.flow_version = rf.flow_version \
              WHERE rf.tenant_id = r.tenant_id AND rf.flow_id = r.flow_id \
                AND rf.flow_version = r.flow_version \
                AND r.tenant_id = current_setting('app.tenant', true) AND r.run_id = $1",
            &[&run_id, &ENVIRONMENT],
        )
        .await
        .context("pin the seeded run to its release member")?;
    anyhow::ensure!(
        pinned == 1,
        "run {run_id} was not release-pinned (no published member for its flow?)"
    );
    Ok(())
}

/// A fixture a gate publishes must be a graph a REAL release could carry:
/// [`Artifact::new`] parses it against the current flow schema and validates it
/// (entry kind, `respond` legality and config, and resolved interfaces). Each
/// gate names this over the fixtures it publishes: an edit that makes one
/// unpublishable then fails a named test instead of that gate's run-next leg in
/// a cluster.
#[cfg(test)]
pub(crate) fn assert_releasable(fixture: &str, flow_json: &str) {
    if let Err(error) = member("gate-drift-tenant", flow_json) {
        panic!("{fixture} is not publishable as a release member: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `<alias>.<name>` a statement names under one alias prefix.
    fn qualified_names(sql: &str, prefix: &str) -> std::collections::BTreeSet<String> {
        sql.match_indices(prefix)
            .map(|(at, _)| {
                sql[at + prefix.len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect::<String>()
            })
            .filter(|name| !name.is_empty())
            .collect()
    }

    /// The `poc-receipt` fixtures `runnerbench`, `logbench`'s runpath leg, and
    /// `failoverbench`'s claim mode all publish through this module.
    #[test]
    fn the_shared_receipt_fixtures_are_releasable_graphs() {
        assert_releasable("flowbench::flow_json(1)", &crate::flowbench::flow_json(1));
        assert_releasable("flowbench::flow_json(2)", &crate::flowbench::flow_json(2));
    }

    /// wamn-kex2 [GATE-DRIFT]: the wamn-jflp/wamn-thvs derived-guard class reads
    /// the `r.`-aliased run-next builders in `wamn-run-state`, so it is blind to
    /// the CATALOG side of the run-next path — the join that made four gates die
    /// `42P01 catalog.release_flows` with no named test to show for it.
    /// `NODE_INVOCATION_SNAPSHOT_SQL` is the host-side statement of record for
    /// that join (`source_run.`-aliased). Derive the relations and run columns it
    /// demands, and hold this module's preamble and the shared run-plane stand-in
    /// to them, so the next change there breaks a named test instead of four gates
    /// in a cluster.
    #[test]
    fn preamble_satisfies_the_run_next_catalog_join() {
        let sql = wamn_runtime::plugins::wamn_postgres::NODE_INVOCATION_SNAPSHOT_SQL;

        let relations = qualified_names(sql, "catalog.");
        assert!(
            relations.contains("release_flows") && relations.contains("flow_artifacts"),
            "parser sanity: the snapshot SQL names the release lineage"
        );
        let missing: Vec<&String> = relations
            .iter()
            .filter(|relation| !CATALOG_DDL.contains(&format!("CREATE TABLE catalog.{relation} (")))
            .collect();
        assert!(
            missing.is_empty(),
            "the catalog preamble omits relations the run-next join reads: {missing:?}"
        );

        let columns = qualified_names(sql, "source_run.");
        assert!(
            columns.contains("invocation_context") && columns.contains("catalog_id"),
            "parser sanity: the snapshot SQL names the run's pinning columns"
        );
        let ddl = crate::runnerbench::runner_ddl("wamn_run");
        let missing: Vec<&String> = columns
            .iter()
            .filter(|column| !ddl.contains(column.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "the run-plane stand-in omits run columns the catalog join reads: {missing:?}"
        );
    }
}
