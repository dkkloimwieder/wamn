//! Process-boundary proof for rev18 cron attachment admission.

use anyhow::{Context as _, ensure};
use clap::Args;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{Artifact, NodeImplementation};
use wamn_flow::Flow;
use wamn_scheduler::{canonical_tick, mint_cron_run_id};

use crate::dispatcher_process::{DispatcherProcess, ProjectSpec};

const TENANT: &str = "callable-cron-gate-v2";
const CATALOG: &str = "callable-cron";
const FLOW: &str = "callable-cron-flow";
const ATTACHMENT: &str = "callable-cron-attachment";
const PROOF_CLOCK_MARGIN_MS: i64 = 60 * 60 * 1_000;

fn proof_base_ms(database_now_ms: i64) -> anyhow::Result<i64> {
    database_now_ms
        .checked_add(PROOF_CLOCK_MARGIN_MS + 999)
        .map(|value| value / 1_000 * 1_000)
        .context("callable cron proof clock overflow")
}

#[derive(Debug, Args)]
pub struct CallableCronArgs {
    /// App-role URL used by the dispatcher and proof assertions.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// Superuser URL used only to seed the immutable proof definition.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,
}

async fn connect(url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect PostgreSQL")?;
    let handle = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, handle))
}

async fn seed(admin: &mut Client) -> anyhow::Result<()> {
    let graph = serde_json::json!({
        "schema-version": "0.1",
        "flow-id": FLOW,
        "version": 1,
        "nodes": [{"id": "entry", "type": "cron"}]
    })
    .to_string();
    let flow = Flow::from_json(&graph)
        .map_err(|error| anyhow::anyhow!("parse callable cron flow: {error}"))?;
    let descriptor = wamn_standard_nodes::describe("cron")
        .context("missing standard-node descriptor for cron")?;
    let contract =
        wamn_standard_nodes::resolve_descriptor(descriptor).map_err(anyhow::Error::new)?;
    let implementation = NodeImplementation::from_resolved_platform_contract(contract)
        .map_err(anyhow::Error::new)?;
    let artifact = Artifact::new(TENANT, &flow, vec![implementation])?;
    let canonical_graph =
        String::from_utf8(flow.canonical_bytes()).expect("canonical cron graph is UTF-8");
    let interfaces = String::from_utf8(artifact.interface_bundle().canonical_bytes().to_vec())
        .expect("canonical cron interfaces are UTF-8");
    let components = serde_json::to_string(artifact.supplied_components())?;
    let occurrence_recovery = String::from_utf8(artifact.occurrence_recovery_bytes().to_vec())
        .expect("canonical cron occurrence recovery is UTF-8");
    let artifact_hash = artifact.identity().artifact_hash().as_str().to_owned();
    let members = serde_json::json!([{
        "flow-id": FLOW,
        "flow-version": 1,
        "artifact-hash": artifact_hash,
    }])
    .to_string();
    admin
        .execute(
            wamn_schema_control::sql::register_flow_artifact_sql(),
            &[
                &TENANT,
                &FLOW,
                &1_i32,
                &"0.1",
                &canonical_graph,
                &artifact.graph_hash(),
                &artifact_hash,
                &interfaces,
                &artifact.interface_bundle().hash(),
                &components,
                &occurrence_recovery,
                &artifact.occurrence_recovery_hash(),
            ],
        )
        .await?;
    admin
        .execute(
            "INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ($1,$2,1,'gate','0.1','applied') \
             ON CONFLICT (tenant_id,catalog_id,version) DO NOTHING",
            &[&TENANT, &CATALOG],
        )
        .await?;
    let release = admin.transaction().await?;
    release
        .execute(
            wamn_schema_control::sql::register_release_manifest_sql(),
            &[&TENANT, &CATALOG, &1_i32, &members],
        )
        .await?;
    release
        .execute(
            "INSERT INTO catalog.release_flows \
               (tenant_id,catalog_id,catalog_version,flow_id,flow_version) \
             VALUES ($1,$2,1,$3,1) ON CONFLICT DO NOTHING",
            &[&TENANT, &CATALOG, &FLOW],
        )
        .await?;
    release.commit().await?;
    admin
        .execute(
            "INSERT INTO catalog.release_exposure_manifests \
               (tenant_id,catalog_id,catalog_version,definitions_json) \
             VALUES ($1,$2,1,'{}') ON CONFLICT DO NOTHING",
            &[&TENANT, &CATALOG],
        )
        .await?;
    admin
        .execute(
            "INSERT INTO catalog.release_sources \
               (tenant_id,catalog_id,catalog_version,source_id,source_kind,definition_json,source_hash) \
             VALUES ($1,$2,1,'schedule','schedule', \
               '{\"schedule\":\"* * * * * *\",\"timezone\":\"UTC\",\"catch-up\":\"skip\"}', \
               'cron-source') ON CONFLICT DO NOTHING",
            &[&TENANT, &CATALOG],
        )
        .await?;
    admin
        .execute(
            "INSERT INTO catalog.release_attachments \
               (tenant_id,catalog_id,catalog_version,attachment_id,attachment_kind,flow_id, \
                source_id,definition_hash,definition_json) \
             VALUES ($1,$2,1,$3,'cron',$4,'schedule','sha256:callable-cron', \
               '{\"id\":\"callable-cron-attachment\",\"kind\":\"cron\", \
                 \"flow-id\":\"callable-cron-flow\",\"source-id\":\"schedule\", \
                 \"run-deadline-ms\":60000}') ON CONFLICT DO NOTHING",
            &[&TENANT, &CATALOG, &ATTACHMENT, &FLOW],
        )
        .await?;
    admin
        .execute(
            "INSERT INTO catalog.catalog_heads \
               (tenant_id,catalog_id,environment,applied_catalog_version) \
             VALUES ($1,$2,'gate',1) \
             ON CONFLICT (tenant_id,catalog_id,environment) \
             DO UPDATE SET applied_catalog_version=EXCLUDED.applied_catalog_version",
            &[&TENANT, &CATALOG],
        )
        .await?;
    admin
        .execute(
            "INSERT INTO catalog.attachment_activation \
               (tenant_id,catalog_id,environment,attachment_id,confirmed_definition_hash,enabled) \
             VALUES ($1,$2,'gate',$3,'sha256:callable-cron',true) \
             ON CONFLICT (tenant_id,catalog_id,environment,attachment_id) \
             DO UPDATE SET confirmed_definition_hash=EXCLUDED.confirmed_definition_hash, \
                           enabled=EXCLUDED.enabled",
            &[&TENANT, &CATALOG, &ATTACHMENT],
        )
        .await?;
    for table in ["run_queue", "node_runs", "runs", "cron_anchor"] {
        admin
            .execute(
                &format!("DELETE FROM wamn_run.{table} WHERE tenant_id=$1"),
                &[&TENANT],
            )
            .await?;
    }
    Ok(())
}

pub async fn run(args: CallableCronArgs) -> anyhow::Result<()> {
    let (mut admin, admin_connection) = connect(&args.admin_database_url).await?;
    seed(&mut admin)
        .await
        .context("seed callable cron definition")?;
    let database_now_ms: i64 = admin
        .query_one(
            "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
            &[],
        )
        .await?
        .get(0);
    let base_ms = proof_base_ms(database_now_ms)?;
    let spec = ProjectSpec {
        name: "callable-cron".to_string(),
        url: args.database_url.clone(),
        tenant: TENANT.to_string(),
        schema: Some("wamn_run".to_string()),
    };
    let mut dispatcher =
        DispatcherProcess::spawn(&[spec], "nats://127.0.0.1:1", None, None, None, None)?;

    let first = dispatcher.tick_project(0, base_ms).await?;
    ensure!(
        first.cron_fired.is_empty(),
        "first sight fired retroactively"
    );
    let tick = base_ms + 1_000;
    let fired_at = tick + 250;
    let fired = dispatcher.tick_project(0, fired_at).await?;
    let expected = mint_cron_run_id(FLOW, tick);
    ensure!(
        fired.cron_fired == [expected.clone()],
        "due tick did not fire once"
    );
    let duplicate = dispatcher.tick_project(0, fired_at + 100).await?;
    ensure!(duplicate.cron_fired.is_empty(), "same tick fired twice");

    let (app, app_connection) = connect(&args.database_url).await?;
    app.batch_execute("SET app.tenant='callable-cron-gate'; SET search_path=wamn_run,public")
        .await?;
    let row = app
        .query_one(
            "SELECT r.catalog_id,r.catalog_version,r.attachment_id,r.trigger_source, \
                    r.input_json->>'scheduled-at',r.input_json->>'fired-at', \
                    q.lease_owner,q.lease_generation \
               FROM runs r JOIN run_queue q USING (tenant_id,run_id) \
              WHERE r.run_id=$1",
            &[&expected],
        )
        .await?;
    ensure!(row.get::<_, String>(0) == CATALOG, "catalog identity drift");
    ensure!(row.get::<_, i64>(1) == 1, "catalog version drift");
    ensure!(
        row.get::<_, String>(2) == ATTACHMENT,
        "attachment identity drift"
    );
    ensure!(row.get::<_, String>(3) == "cron", "producer identity drift");
    ensure!(
        row.get::<_, String>(4) == canonical_tick(tick)?,
        "scheduled-at drift"
    );
    ensure!(
        row.get::<_, String>(5) == canonical_tick(fired_at)?,
        "fired-at drift"
    );
    ensure!(
        row.get::<_, Option<String>>(6).is_none() && row.get::<_, i64>(7) == 0,
        "cron admission was not available and unclaimed"
    );

    admin
        .execute(
            "UPDATE catalog.attachment_activation SET enabled=false \
              WHERE tenant_id=$1 AND catalog_id=$2 AND environment='gate' AND attachment_id=$3",
            &[&TENANT, &CATALOG, &ATTACHMENT],
        )
        .await?;
    let disabled = dispatcher.tick_project(0, tick + 2_000).await?;
    ensure!(
        disabled.cron_fired.is_empty(),
        "disabled attachment admitted a run"
    );
    admin
        .execute(
            "UPDATE catalog.attachment_activation SET enabled=true \
              WHERE tenant_id=$1 AND catalog_id=$2 AND environment='gate' AND attachment_id=$3",
            &[&TENANT, &CATALOG, &ATTACHMENT],
        )
        .await?;

    drop(app);
    app_connection.abort();
    drop(admin);
    admin_connection.abort();
    println!("callable cron attachment/admission proof PASS");
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    const SOURCE: &str = include_str!("callable_cron.rs");
    const DISPATCHER: &str = include_str!("../../../services/dispatcher/src/lib.rs");
    const MANIFEST: &str = include_str!("../../../deploy/gates/callable-flow-cron-job.yaml");

    #[test]
    fn proof_uses_catalog_attachment_and_process_boundary() {
        assert!(SOURCE.contains("catalog.release_attachments"));
        assert!(SOURCE.contains("Artifact::new"));
        assert!(SOURCE.contains("register_flow_artifact_sql"));
        let placeholder_artifact = ["cron", "artifact"].join("-");
        assert!(!SOURCE.contains(&placeholder_artifact));
        assert!(SOURCE.contains("DispatcherProcess::spawn"));
        assert!(!DISPATCHER.contains("INSERT INTO wamn_run.runs"));
        assert!(MANIFEST.contains("callable-cron"));
    }

    #[test]
    fn dispatcher_binds_event_lineage_nulls_before_cron_partition_fields() {
        assert!(DISPATCHER.contains(
            "&no_text,\n                &no_text,\n                &no_text,\n                &no_text,\n                &no_i32,\n                &partition_key,\n                &policy,"
        ));
    }

    #[test]
    fn proof_clock_is_second_aligned_and_ahead_of_database_time() {
        let database_now_ms = 1_767_225_600_250;
        let base_ms = super::proof_base_ms(database_now_ms).unwrap();
        assert_eq!(base_ms, 1_767_229_201_000);
        assert!(base_ms >= database_now_ms + super::PROOF_CLOCK_MARGIN_MS);
        assert_eq!(base_ms % 1_000, 0);
    }
}
