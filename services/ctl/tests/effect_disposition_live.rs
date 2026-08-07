//! Live proof for the privileged ctl effect-disposition adapter and run view.

use tokio_postgres::NoTls;
use wamn_ctl::effect_disposition::{
    DispositionActionArg, EffectDispositionBreakGlassArgs, EffectDispositionViewArgs, run, view,
};

#[tokio::test]
#[ignore = "requires WAMN_CTL_PG_URL and a throwaway PostgreSQL database"]
async fn effect_disposition_break_glass_and_view_live() {
    let url = std::env::var("WAMN_CTL_PG_URL")
        .expect("set WAMN_CTL_PG_URL to the throwaway superuser database");
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let catalog = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .expect("read catalog DDL");
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .expect("read run-state DDL");
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .expect("read run-queue DDL");

    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(&format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
             END $$; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             {catalog} {run_state} {run_queue} \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,status,state_json) \
             VALUES ('t1','ctl-disp','f',1,'running','{{}}'); \
             INSERT INTO wamn_run.run_queue \
               (tenant_id,run_id,available_at,lease_generation) \
             VALUES ('t1','ctl-disp',now(),1); \
             INSERT INTO wamn_run.effect_attempts \
               (tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index, \
                selected_recovery_class,recovery_class,generation_fact_kind, \
                attempt_started_at,attempt_deadline_at,attempt_input_ref) \
             VALUES ('t1','42000000-0000-0000-0000-000000000001', \
                     'ctl-disp','effect',0,0,0,'never-replay','never-replay', \
                     'not-required',now(),now()+interval '1 minute','sha256:input'); \
             INSERT INTO wamn_run.node_runs \
               (tenant_id,current_effect_attempt_id,run_id,node_id,occurrence,seq,status) \
             VALUES ('t1','42000000-0000-0000-0000-000000000001', \
                     'ctl-disp','effect',0,0,'started'); \
             INSERT INTO wamn_run.effect_attempt_dispatches(tenant_id,attempt_id) \
             VALUES ('t1','42000000-0000-0000-0000-000000000001');"
        ))
        .await
        .expect("seed uncertain attempt");
    let expected_actor: String = client
        .query_one("SELECT SESSION_USER::text", &[])
        .await
        .expect("read session actor")
        .get(0);

    run(EffectDispositionBreakGlassArgs {
        admin_database_url: url.clone(),
        schema: "wamn_run".to_string(),
        tenant: "t1".to_string(),
        action: DispositionActionArg::Park,
        attempt_id: Some("42000000-0000-0000-0000-000000000001".to_string()),
        connection_name: None,
        connection_generation: None,
        window_start: None,
        window_end: None,
        flow_id: None,
        correlation_id: "ctl-live:42".to_string(),
        reason: "incident commander approved".to_string(),
        basis: None,
        evidence_ref: None,
        resolution_status: None,
        success_payload: None,
        success_port: None,
        success_context: None,
        failure_kind: None,
        failure_detail: None,
    })
    .await
    .expect("apply platform park");

    let audit = client
        .query_one(
            "SELECT principal,effective_role,break_glass_reason \
             FROM wamn_run.effect_disposition_requests \
             WHERE tenant_id='t1' AND correlation_id='ctl-live:42'",
            &[],
        )
        .await
        .expect("read immutable ctl audit");
    assert_eq!(audit.get::<_, String>(0), expected_actor);
    assert_eq!(audit.get::<_, String>(1), "platform-admin-break-glass");
    assert_eq!(
        audit.get::<_, String>(2),
        "incident commander approved"
    );

    view(EffectDispositionViewArgs {
        database_url: url,
        schema: "wamn_run".to_string(),
        tenant: "t1".to_string(),
        run_id: "ctl-disp".to_string(),
    })
    .await
    .expect("render parked run view");
}
