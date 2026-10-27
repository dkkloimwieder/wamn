//! Fresh schemas and populated record-history migrations agree.

use std::fmt::Write as _;

use wamn_control_provision::{
    CONTROL_BOOTSTRAP_SQL, PlatformComponent, bind_platform_principal_sql,
};

const CONTROL: &[(&str, &str)] = &[
    ("catalog.tenant_environments", "projected_at"),
    ("catalog.authoring_command_audit", "recorded_at"),
    ("wamn_run.gate_reports", "gated_at"),
    ("wamn_authority.author_login_tenants", "created_at"),
];
const RUN_STATE: &[(&str, &str)] = &[
    ("wamn_run.environment_policies", "transaction_timestamp()"),
    ("wamn_run.effect_attempts", "created_at"),
    ("wamn_run.effect_attempt_dispatches", "dispatched_at"),
    ("wamn_run.effect_attempt_outcomes", "recorded_at"),
    ("wamn_run.operator_run_actions", "created_at"),
];
const QUEUE: &[(&str, &str)] = &[("wamn_run.run_queue", "enqueued_at")];

fn shape(database: &wamn_test_postgres::Database, relations: &[(&str, &str)]) -> String {
    let names = relations
        .iter()
        .map(|(name, _)| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(",");
    database.execute(&[&format!(
        "WITH relations AS (SELECT unnest(ARRAY[{names}])::regclass AS oid) \
         SELECT entry FROM ( \
           SELECT a.attrelid::regclass || ' column ' || a.attname || ' ' || \
             format_type(a.atttypid,a.atttypmod) || ' ' || a.attnotnull || ' ' || \
             coalesce(pg_get_expr(d.adbin,d.adrelid),'') AS entry \
           FROM pg_attribute a JOIN relations r ON r.oid=a.attrelid \
           LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum \
           WHERE a.attnum>0 AND NOT a.attisdropped \
           UNION ALL SELECT conrelid::regclass || ' constraint ' || conname || ' ' || \
             pg_get_constraintdef(c.oid) FROM pg_constraint c JOIN relations r ON r.oid=c.conrelid \
           UNION ALL SELECT tgrelid::regclass || ' trigger ' || pg_get_triggerdef(t.oid) \
           FROM pg_trigger t JOIN relations r ON r.oid=t.tgrelid WHERE NOT t.tgisinternal \
           UNION ALL SELECT c.oid::regclass || ' rls ' || relrowsecurity || ' ' || relforcerowsecurity \
           FROM pg_class c JOIN relations r ON r.oid=c.oid \
         ) entries ORDER BY entry"
    )]).unwrap()
}

fn backfill(
    database: &wamn_test_postgres::Database,
    relations: &[(&str, &str)],
    migration: &str,
    seed: &str,
) {
    let fresh = shape(database, relations);
    let mut restore = String::new();
    for (relation, _) in relations {
        let created = if matches!(
            *relation,
            "wamn_authority.author_login_tenants"
                | "wamn_run.effect_attempts"
                | "wamn_run.operator_run_actions"
        ) {
            "ALTER COLUMN created_at SET DEFAULT now()"
        } else {
            "DROP COLUMN created_at"
        };
        write!(restore, "DROP TRIGGER wamn_record_history_stamp ON {relation}; ALTER TABLE {relation} {created}, DROP COLUMN created_by, DROP COLUMN updated_at, DROP COLUMN updated_by;").unwrap();
    }
    database.execute(&[&restore, seed]).unwrap();
    let actor = PlatformComponent::Provisioning.principal_id();
    let mut assertions = String::new();
    for (relation, time) in relations {
        write!(assertions,
            "DO $assert$ BEGIN ASSERT (SELECT count(*)>0 AND bool_and(created_by='{actor}'::uuid AND updated_by='{actor}'::uuid AND created_at={time} AND updated_at=created_at AND isfinite(created_at)) FROM {relation}), '{relation}: backfill'; END $assert$;"
        ).unwrap();
    }
    let role = if relations == CONTROL {
        "SET LOCAL ROLE wamn_system"
    } else {
        "RESET ROLE"
    };
    database
        .execute(&[
            "BEGIN",
            role,
            migration,
            "RESET ROLE",
            &assertions,
            "COMMIT",
        ])
        .unwrap();
    assert_eq!(shape(database, relations), fresh);
}

#[test]
fn control_history_migration_matches_fresh_schema_and_backfills_rows() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    database.execute(&[
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$;",
        &wamn_control_provision::sql::ensure_control_author_acl_role_sql(),
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $$ BEGIN EXECUTE format('ALTER DATABASE %I OWNER TO wamn_system',current_database()); END $$; SET ROLE wamn_system;",
        CONTROL_BOOTSTRAP_SQL[0], CONTROL_BOOTSTRAP_SQL[1], "RESET ROLE",
    ]).unwrap();
    backfill(
        &database,
        CONTROL,
        include_str!(
            "../../../../deploy/sql/migrations/system/0034_control_record_history_stamp.sql"
        ),
        r"
        INSERT INTO catalog.tenant_environments (tenant_id,org,project,env,instance_suffix,disposable,environment_instance) VALUES ('history','org','project','dev','abcdefgh',false,'');
        INSERT INTO catalog.authoring_command_audit (tenant_id,command_id,command_type,principal_id,principal_type,principal_subject,effective_role,org,project,environment,target_ref,request_hash,outcome_bytes) VALUES ('history','command','gate','actor','user','actor','project-author','org','project','dev','target','sha256:'||repeat('a',64),'outcome');
        INSERT INTO wamn_run.gate_reports (tenant_id,wiring_hash,passed,summary) VALUES ('history','sha256:'||repeat('a',64),true,'{}');
        INSERT INTO wamn_authority.author_login_tenants (login_identity,tenant_id,org_id,project_id,environment) VALUES ('login','history','org','project','dev');
    ",
    );
    // The production SQL writers stamp their rows with the transaction actor.
    database.execute(&["BEGIN", &bind_platform_principal_sql(PlatformComponent::Provisioning), "SET LOCAL app.tenant='history';", &format!("PREPARE projection(text,text,text,text,text,boolean) AS {}; EXECUTE projection('production','org','project','dev','abcdefgh',false);", wamn_control_provision::sql::insert_tenant_environment_sql()), &format!("PREPARE mapping(text,text,text,text,text) AS {}; EXECUTE mapping('production-login','production','org','project','dev');", wamn_control_provision::sql::upsert_control_author_tenant_mapping_sql()), "COMMIT"]).unwrap();
    let actor = PlatformComponent::Provisioning.principal_id();
    for (relation, predicate) in [
        ("catalog.tenant_environments", "tenant_id='production'"),
        (
            "wamn_authority.author_login_tenants",
            "login_identity='production-login'",
        ),
    ] {
        database.execute(&[&format!("DO $$ BEGIN ASSERT (SELECT count(*)=1 AND bool_and(created_by='{actor}'::uuid AND updated_by='{actor}'::uuid AND created_at=updated_at AND isfinite(created_at)) FROM {relation} WHERE {predicate}); END $$;")]).unwrap();
    }
}

#[test]
fn run_history_migrations_match_fresh_schema_and_backfill_rows() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    database
        .execute(&[
            &wamn_control_provision::sql::ensure_app_acl_role_sql(),
            wamn_schema_control::ensure_scenario_author_role_sql(),
            wamn_catalog::CATALOG_SCHEMA_SQL,
            include_str!("../../../../deploy/sql/run-state.sql"),
            include_str!("../../../../deploy/sql/run-queue.sql"),
        ])
        .unwrap();
    backfill(
        &database,
        RUN_STATE,
        include_str!(
            "../../../../deploy/sql/migrations/project/0043_run_state_record_history_stamp.sql"
        ),
        r"
        INSERT INTO wamn_run.environment_policies (tenant_id,expected_environment,durability_class) VALUES ('history','dev','standard');
        INSERT INTO wamn_run.runs (tenant_id,run_id,flow_id,flow_version,package_id,environment) VALUES ('history','run','flow',1,'package','dev');
        INSERT INTO wamn_run.effect_attempts (tenant_id,attempt_id,run_id,root_plan_hash,current_plan_hash,local_node_id,source_artifact_hash,requirement_name,occurrence,seq,generation_fact_type,attempt_deadline_at,attempt_input_ref) VALUES ('history','11111111-1111-4111-8111-111111111111','run','sha256:'||repeat('a',64),'sha256:'||repeat('a',64),'node','sha256:'||repeat('a',64),'database',0,0,'not-required',now()+interval '1 hour','input');
        INSERT INTO wamn_run.effect_attempt_dispatches (tenant_id,attempt_id,attempt_started_at,run_id,frame_id,local_node_id,occurrence) SELECT tenant_id,attempt_id,attempt_started_at,run_id,frame_id,local_node_id,occurrence FROM wamn_run.effect_attempts;
        INSERT INTO wamn_run.effect_attempt_outcomes (tenant_id,attempt_id,dispatched_at,outcome_status) SELECT tenant_id,attempt_id,dispatched_at,'success' FROM wamn_run.effect_attempt_dispatches;
        INSERT INTO wamn_run.operator_run_actions (tenant_id,correlation_id,run_id,action_type,basis,evidence_ref,principal,principal_type,prior_run_status) VALUES ('history','action','run','terminalize-effect-uncertain','external-evidence','case','operator','database-role','effect-uncertain');
    ",
    );
    backfill(
        &database,
        QUEUE,
        include_str!(
            "../../../../deploy/sql/migrations/project/0044_run_queue_record_history_stamp.sql"
        ),
        "INSERT INTO wamn_run.run_queue(tenant_id,run_id) VALUES ('history','run');",
    );
}
