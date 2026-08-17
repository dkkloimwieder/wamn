//! Drift and optional live-apply proof for the operations persistence extension.
//!
//! The core control schema remains independently installable. The ops artifact
//! is applied afterwards, owns exactly three operations relations, and may
//! reference only the core project-environment identity.

use std::path::Path;

fn deploy_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy")
}

fn system_schema_sql() -> String {
    std::fs::read_to_string(deploy_dir().join("sql/system-schema.sql"))
        .expect("read deploy/sql/system-schema.sql")
}

fn ops_schema_sql() -> String {
    std::fs::read_to_string(deploy_dir().join("sql/ops-schema.sql"))
        .expect("read deploy/sql/ops-schema.sql")
}

#[test]
fn packaged_ops_schema_is_the_deploy_artifact() {
    assert_eq!(wamn_control_provision::OPS_SCHEMA_SQL, ops_schema_sql());
}

/// Strip `--` comments before asserting contract-bearing SQL text.
fn code_only(sql: &str) -> String {
    sql.lines()
        .map(|line| line.find("--").map_or(line, |index| &line[..index]))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn core_schema_has_no_operations_relations_or_literals() {
    let core = code_only(&system_schema_sql());

    for operations_only in [
        "provisioning.dumps",
        "provisioning.copy_sagas",
        "provisioning.migration_confirmations",
        "'copy'",
        "'backup-checkpoint-attested'",
    ] {
        assert!(
            !core.contains(operations_only),
            "core schema contains operations-only SQL {operations_only:?}"
        );
    }
}

#[test]
fn ops_schema_is_additive_idempotent_and_one_way() {
    let ops = code_only(&ops_schema_sql());

    assert_eq!(
        ops.matches("CREATE TABLE IF NOT EXISTS provisioning.")
            .count(),
        3,
        "the ops artifact owns exactly its three additive relations"
    );
    for relation in ["dumps", "copy_sagas", "migration_confirmations"] {
        assert!(
            ops.contains(&format!(
                "CREATE TABLE IF NOT EXISTS provisioning.{relation}"
            )),
            "ops artifact is missing provisioning.{relation}"
        );
    }

    assert!(
        !ops.contains("CREATE SCHEMA") && !ops.contains("CREATE TABLE registry."),
        "the ops artifact must extend an installed core schema, never install core objects"
    );
    assert_eq!(
        ops.matches("REFERENCES registry.project_envs").count(),
        2,
        "dumps and migration confirmations are the only ops-to-core references"
    );
    for forbidden_reference in [
        "REFERENCES provisioning.sagas",
        "REFERENCES identity.",
        "REFERENCES registry.orgs",
        "REFERENCES registry.projects",
        "REFERENCES registry.env_policies",
        "REFERENCES registry.event_readers",
    ] {
        assert!(
            !ops.contains(forbidden_reference),
            "unexpected cross-boundary reference {forbidden_reference:?}"
        );
    }

    let role = wamn_control_provision::state::ensure_ops_role_sql();
    for attribute in [
        "NOLOGIN",
        "NOSUPERUSER",
        "NOCREATEDB",
        "NOCREATEROLE",
        "NOINHERIT",
        "NOREPLICATION",
        "NOBYPASSRLS",
    ] {
        assert!(role.contains(attribute), "wamn_ops role lost {attribute}");
    }
    assert!(ops.contains("GRANT USAGE ON SCHEMA provisioning TO wamn_ops"));
    assert!(ops.contains("GRANT SELECT, INSERT, UPDATE ON provisioning.dumps TO wamn_ops"));
    assert!(ops.contains("GRANT SELECT, INSERT, UPDATE ON provisioning.copy_sagas TO wamn_ops"));
    assert!(ops.contains("GRANT SELECT ON provisioning.migration_confirmations TO wamn_ops"));
    assert!(ops.contains(
        "GRANT INSERT (org, project, env, tenant_id, catalog_id, from_version, to_version)"
    ));
    assert!(
        !ops.contains("UPDATE ON provisioning.migration_confirmations")
            && !ops.contains("DELETE ON provisioning.migration_confirmations")
            && !ops.contains("TRUNCATE ON provisioning.migration_confirmations"),
        "migration confirmations must remain append-only for wamn_ops"
    );
}

#[test]
fn copy_and_dump_builders_match_the_ops_relations() {
    let ops = code_only(&ops_schema_sql());

    let create = wamn_control_provision::state::create_saga_sql();
    assert!(create.contains("INSERT INTO provisioning.copy_sagas"));
    assert!(create.contains("ON CONFLICT (saga_id) DO NOTHING"));
    for column in ["saga_id", "kind", "target", "total_steps"] {
        assert!(
            create.contains(column),
            "copy-saga builder missing {column}"
        );
        assert!(ops.contains(column), "copy-saga DDL missing {column}");
    }
    for builder in [
        wamn_control_provision::state::advance_saga_step_sql(),
        wamn_control_provision::state::complete_saga_sql(),
        wamn_control_provision::state::fail_saga_sql(),
        wamn_control_provision::state::select_saga_sql(),
    ] {
        assert!(
            builder.contains("provisioning.copy_sagas"),
            "copy state builder targets a non-ops relation: {builder}"
        );
    }

    let record = wamn_control_provision::state::record_dump_sql();
    assert!(record.contains("INSERT INTO provisioning.dumps"));
    assert!(record.contains("ON CONFLICT (org, project, env, object_key) DO UPDATE"));
    for reader in [
        wamn_control_provision::state::select_latest_dump_sql(),
        wamn_control_provision::state::select_dumps_sql(),
    ] {
        assert!(reader.contains("FROM provisioning.dumps"));
        assert!(reader.contains("ORDER BY taken_at DESC, object_key DESC"));
    }
}

#[test]
fn migration_confirmation_shape_and_builders_are_frozen() {
    let ops = code_only(&ops_schema_sql());
    let kind = wamn_control_provision::state::MIGRATION_CONFIRMATION_KIND;
    let block = ops
        .split("CREATE TABLE IF NOT EXISTS provisioning.migration_confirmations")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("migration_confirmations table body");

    for column in [
        "org",
        "project",
        "env",
        "tenant_id",
        "catalog_id",
        "from_version",
        "to_version",
        "kind",
        "confirmed_at",
        "confirmed_by",
    ] {
        assert!(block.contains(column), "confirmation DDL missing {column}");
    }
    assert!(block.contains("PRIMARY KEY (org, project, env, tenant_id, catalog_id, to_version)"));
    assert!(block.contains("REFERENCES registry.project_envs (org, project, env)"));
    assert!(block.contains(&format!("DEFAULT '{kind}'")));
    assert!(block.contains(&format!("CHECK (kind = '{kind}')")));
    assert!(block.contains("confirmed_by text NOT NULL DEFAULT session_user"));

    let record = wamn_control_provision::state::record_migration_confirmation_sql();
    assert!(record.contains("INSERT INTO provisioning.migration_confirmations"));
    assert!(
        record.contains("(org, project, env, tenant_id, catalog_id, from_version, to_version)")
    );
    assert!(
        record.contains(
            "ON CONFLICT (org, project, env, tenant_id, catalog_id, to_version) DO NOTHING"
        )
    );
    assert!(record.contains("$7"));
    assert!(!record.contains("$8"));
    assert!(
        !record.contains("confirmed_by") && !record.contains("confirmed_at"),
        "the caller must never provide confirmation actor or time"
    );

    let select = wamn_control_provision::state::select_migration_confirmation_sql();
    assert!(select.contains("FROM provisioning.migration_confirmations"));
    assert!(select.contains("confirmed_by"));
    assert!(select.contains("to_version = $6"));
}

/// Apply core once and the ops extension twice to a throwaway Postgres, then
/// exercise the real builders. Skipped when `WAMN_REGISTRY_PG_URL` is unset.
#[test]
fn ops_schema_applies_idempotently_after_core_on_postgres() {
    let Ok(url) = std::env::var("WAMN_REGISTRY_PG_URL") else {
        eprintln!(
            "skipping ops_schema_applies_idempotently_after_core_on_postgres \
             (set WAMN_REGISTRY_PG_URL to run)"
        );
        return;
    };

    let mut script = String::new();
    script.push_str(wamn_control_provision::state::ensure_ops_role_sql());
    script.push('\n');
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
         CREATE ROLE wamn_system LOGIN PASSWORD 'wamn_system' NOSUPERUSER; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS registry CASCADE;\n\
         DROP SCHEMA IF EXISTS provisioning CASCADE;\n\
         DROP SCHEMA IF EXISTS identity CASCADE;\n\
         DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); END $$;\n\
         SET ROLE wamn_system;\n",
    );
    script.push_str(&system_schema_sql());
    script.push('\n');
    script.push_str(&ops_schema_sql());
    script.push('\n');
    script.push_str(
        "GRANT INSERT (confirmed_by), UPDATE (from_version) \
           ON provisioning.migration_confirmations TO wamn_ops;\n",
    );
    script.push_str(&ops_schema_sql());
    script.push('\n');
    script.push_str(
        "INSERT INTO registry.orgs (id, placement_kind) VALUES ('acme','dedicated');\n\
         INSERT INTO registry.env_policies \
             (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) \
           VALUES ('acme','dev','\"own\"'::jsonb,10,1,'2Gi','200m','256Mi','postgres:18');\n\
         INSERT INTO registry.projects (org,id) VALUES ('acme','app');\n\
         INSERT INTO registry.project_envs (org,project,env,secret_name,instance_suffix) \
           VALUES ('acme','app','dev','wamn-db-acme--app--dev','k3m9x2p7');\n\
         RESET ROLE;\n\
         SET ROLE wamn_ops;\n",
    );
    script.push_str(&format!(
        "PREPARE dump (text,text,text,text,text,bigint) AS {record_dump};\n\
         EXECUTE dump('acme','app','dev','dumps/acme/app/dev/1','directory',10);\n\
         EXECUTE dump('acme','app','dev','dumps/acme/app/dev/1','directory',20);\n\
         PREPARE copy (text,text,text,int) AS {create_copy};\n\
         PREPARE advance (text) AS {advance_copy};\n\
         EXECUTE copy('copy-1','copy','acme/app/dev -> acme/app/prod',5);\n\
         EXECUTE advance('copy-1');\n\
         PREPARE confirm (text,text,text,text,text,int,int) AS {confirm};\n\
         EXECUTE confirm('acme','app','dev','tenant-1','catalog-1',1,2);\n\
         EXECUTE confirm('acme','app','dev','tenant-1','catalog-1',1,2);\n",
        record_dump = wamn_control_provision::state::record_dump_sql(),
        create_copy = wamn_control_provision::state::create_saga_sql(),
        advance_copy = wamn_control_provision::state::advance_saga_step_sql(),
        confirm = wamn_control_provision::state::record_migration_confirmation_sql(),
    ));
    script.push_str(
        "DO $$ BEGIN\n\
           ASSERT (SELECT byte_size FROM provisioning.dumps WHERE object_key='dumps/acme/app/dev/1')=20, \
             'dump upsert updates metadata';\n\
           ASSERT (SELECT step FROM provisioning.copy_sagas WHERE saga_id='copy-1')=1, \
             'copy checkpoint advances';\n\
           ASSERT (SELECT count(*) FROM provisioning.migration_confirmations)=1, \
             'confirmation identity is append-once';\n\
           ASSERT (SELECT kind FROM provisioning.migration_confirmations)='backup-checkpoint-attested', \
             'confirmation kind is schema-minted';\n\
           ASSERT (SELECT confirmed_by FROM provisioning.migration_confirmations)=session_user, \
             'confirmation actor is the authenticated session';\n\
           ASSERT has_schema_privilege('wamn_ops','provisioning','USAGE'), \
             'wamn_ops needs provisioning usage';\n\
           ASSERT has_table_privilege('wamn_ops','provisioning.dumps','SELECT') \
              AND has_table_privilege('wamn_ops','provisioning.dumps','INSERT') \
              AND has_table_privilege('wamn_ops','provisioning.dumps','UPDATE'), \
             'dump ACL drifted';\n\
           ASSERT has_table_privilege('wamn_ops','provisioning.copy_sagas','SELECT') \
              AND has_table_privilege('wamn_ops','provisioning.copy_sagas','INSERT') \
              AND has_table_privilege('wamn_ops','provisioning.copy_sagas','UPDATE'), \
             'copy ACL drifted';\n\
           ASSERT has_table_privilege('wamn_ops','provisioning.migration_confirmations','SELECT') \
              AND NOT has_table_privilege('wamn_ops','provisioning.migration_confirmations','INSERT') \
              AND has_column_privilege('wamn_ops','provisioning.migration_confirmations','org','INSERT') \
              AND has_column_privilege('wamn_ops','provisioning.migration_confirmations','project','INSERT') \
              AND has_column_privilege('wamn_ops','provisioning.migration_confirmations','env','INSERT') \
              AND has_column_privilege('wamn_ops','provisioning.migration_confirmations','tenant_id','INSERT') \
              AND has_column_privilege('wamn_ops','provisioning.migration_confirmations','catalog_id','INSERT') \
              AND has_column_privilege('wamn_ops','provisioning.migration_confirmations','from_version','INSERT') \
              AND has_column_privilege('wamn_ops','provisioning.migration_confirmations','to_version','INSERT') \
              AND NOT has_column_privilege('wamn_ops','provisioning.migration_confirmations','kind','INSERT') \
              AND NOT has_column_privilege('wamn_ops','provisioning.migration_confirmations','confirmed_at','INSERT') \
              AND NOT has_column_privilege('wamn_ops','provisioning.migration_confirmations','confirmed_by','INSERT'), \
             'confirmation append ACL drifted';\n\
           ASSERT NOT has_table_privilege('wamn_ops','provisioning.migration_confirmations','UPDATE') \
              AND NOT has_column_privilege('wamn_ops','provisioning.migration_confirmations','from_version','UPDATE') \
              AND NOT has_table_privilege('wamn_ops','provisioning.migration_confirmations','DELETE') \
              AND NOT has_table_privilege('wamn_ops','provisioning.migration_confirmations','TRUNCATE'), \
             'confirmation relation is append-only for wamn_ops';\n\
           ASSERT NOT has_schema_privilege('wamn_ops','provisioning','CREATE'), \
             'wamn_ops must not create provisioning objects';\n\
           ASSERT (SELECT c.relowner = r.oid FROM pg_class c \
                     JOIN pg_namespace n ON n.oid=c.relnamespace \
                     JOIN pg_roles r ON r.rolname='wamn_system' \
                    WHERE n.nspname='provisioning' AND c.relname='migration_confirmations'), \
             'wamn_system must retain physical ownership';\n\
           ASSERT (SELECT NOT rolcanlogin AND NOT rolsuper AND NOT rolcreatedb \
                            AND NOT rolcreaterole AND NOT rolinherit \
                            AND NOT rolreplication AND NOT rolbypassrls \
                     FROM pg_roles WHERE rolname='wamn_ops'), \
             'wamn_ops security attributes drifted';\n\
         END $$;\n\
         DO $$ BEGIN BEGIN\n\
           UPDATE provisioning.migration_confirmations SET from_version=0;\n\
           ASSERT false, 'wamn_ops must not update migration confirmations';\n\
         EXCEPTION WHEN insufficient_privilege THEN NULL; END; END $$;\n\
         DO $$ BEGIN BEGIN\n\
           INSERT INTO provisioning.migration_confirmations \
               (org,project,env,tenant_id,catalog_id,to_version,kind) \
             VALUES ('acme','app','dev','tenant-2','catalog-1',2,'legacy-cli-ack');\n\
           ASSERT false, 'wamn_ops must not supply schema-minted confirmation facts';\n\
         EXCEPTION WHEN insufficient_privilege THEN NULL; END; END $$;\n\
         DO $$ BEGIN BEGIN\n\
           INSERT INTO provisioning.dumps (org,project,env,object_key) \
             VALUES ('acme','missing','dev','dumps/acme/missing/dev/1');\n\
           ASSERT false, 'ops-to-core identity FK must reject an unknown project-env';\n\
         EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;\n\
         DEALLOCATE dump; DEALLOCATE copy; DEALLOCATE advance; DEALLOCATE confirm;\n\
         RESET ROLE;\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT string_agg(table_name,',' ORDER BY table_name) \
                     FROM information_schema.tables \
                     WHERE table_schema='provisioning' AND table_type='BASE TABLE') = \
                  'copy_sagas,dumps,migration_confirmations,sagas', \
             'core plus ops provisioning relation set';\n\
         END $$;\n\
         DROP SCHEMA registry CASCADE;\n\
         DROP SCHEMA provisioning CASCADE;\n\
",
    );

    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write psql script");
    let output = child.wait_with_output().expect("wait for psql");
    assert!(
        output.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&output.stderr)
    );
}
