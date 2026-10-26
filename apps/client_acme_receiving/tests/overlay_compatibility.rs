//! The unchanged Acme overlay of a candidate release against a fresh
//! installation of its base.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_control::apply_package::{self, ApplyPackageRequest};
use wamn_control::package_artifact::OpenedPackage;
use wamn_engine::component_admission::component_digest;
use wamn_schema_introspection::ir::{
    Constraint, ForeignKeyAction, ForeignKeyColumn, postgres_type,
};

use super::identity;

/// Observe the schema that the candidate's overlay package requires in the
/// project database of `project_url`, where the candidate is installed, and
/// write the evidence to the new file `evidence_file`. The overlay is the
/// candidate's package artifact and its component the digest the release
/// manifest names (docs/plan/platform-deploy.md §8.2).
pub(super) async fn unchanged_overlay(
    evidence_file: &Path,
    project_url: &str,
    overlay: &OpenedPackage,
    overlay_component_digest: &str,
) -> anyhow::Result<()> {
    let required = required_schema_contract(overlay.root())?;
    let (project, connection_task) = super::connect(project_url).await?;
    let observation = observe_installed_contract(&project, &required).await;
    drop(project);
    connection_task
        .await
        .context("join installed schema observer")?;
    let observed = observation?;
    let requirements = required_contract_observation(&required, &observed)?;
    let evidence = json!({
        "case":"unchanged-overlay-compatibility", "invariant":"OVL-SCHEMA",
        "stage":"installed-schema-observed",
        "overlay_package_artifact":overlay.package_root().artifact_digest,
        "overlay_component_sha256":overlay_component_digest,
        "schema_observation":requirements,
        "observed_schema_sha256":component_digest(&serde_json::to_vec(&observed)?),
        "required_schema_contract":required,
        "schema_admission_claim":false,
    });
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(evidence_file)
        .context("create compatibility evidence without replacing an earlier run")?;
    serde_json::to_writer_pretty(&mut file, &evidence)?;
    file.write_all(b"\n")?;
    Ok(())
}

// This projection observes installed facts, including granted schemas. It does
// not admit authoring input or construct a complete authoring catalog identity.
async fn observe_installed_contract(
    project: &tokio_postgres::Client,
    required: &Value,
) -> anyhow::Result<Value> {
    let mut tables = Vec::new();
    for table in required["tables"]
        .as_array()
        .context("required tables must be an array")?
    {
        let schema = table["schema"]
            .as_str()
            .context("required schema must be a name")?;
        let name = table["table"]
            .as_str()
            .context("required table must be a name")?;
        let rows = project
            .query(wamn_schema_control::select_schema_columns_sql(), &[&schema])
            .await?;
        let mut columns = Vec::new();
        for row in rows.iter().filter(|row| row.get::<_, &str>(0) == name) {
            columns.push(json!({
                "name":row.get::<_, &str>(1),
                "type":postgres_type(row.get::<_, &str>(4))?.as_str(),
                "nullable":!row.get::<_, bool>(2),
            }));
        }
        ensure!(
            !columns.is_empty(),
            "unsatisfied overlay schema requirement: {schema}.{name}"
        );
        let mut constraints = Vec::new();
        for constraint in table["constraints"]
            .as_array()
            .context("required constraints must be an array")?
        {
            let constraint_name = constraint["name"]
                .as_str()
                .context("required constraint must be a name")?;
            // Ordered key pairs and pg_get_expr(false) match the existing IR
            // representation; rendered pg_get_constraintdef text does not.
            let row = project.query_opt(
                "SELECT con.contype::text, \
                    ARRAY(SELECT att.attname::text FROM unnest(con.conkey) WITH ORDINALITY key(attnum, position) \
                          JOIN pg_catalog.pg_attribute att ON att.attrelid = con.conrelid AND att.attnum = key.attnum \
                          ORDER BY key.position), \
                    referenced_namespace.nspname::text, referenced_relation.relname::text, \
                    ARRAY(SELECT att.attname::text FROM unnest(con.confkey) WITH ORDINALITY key(attnum, position) \
                          JOIN pg_catalog.pg_attribute att ON att.attrelid = con.confrelid AND att.attnum = key.attnum \
                          ORDER BY key.position), \
                    con.confupdtype::text, con.confdeltype::text, \
                    pg_catalog.pg_get_expr(con.conbin, con.conrelid, false), con.conenforced, con.convalidated, con.condeferrable, con.condeferred \
                 FROM pg_catalog.pg_constraint con \
                 JOIN pg_catalog.pg_class relation ON relation.oid = con.conrelid \
                 JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
                 LEFT JOIN pg_catalog.pg_class referenced_relation ON referenced_relation.oid = con.confrelid \
                 LEFT JOIN pg_catalog.pg_namespace referenced_namespace ON referenced_namespace.oid = referenced_relation.relnamespace \
                 WHERE namespace.nspname = $1 AND relation.relname = $2 AND con.conname = $3 AND relation.relkind = 'r'",
                &[&schema, &name, &constraint_name],
            ).await?.with_context(|| format!("unsatisfied overlay schema requirement: {schema}.{name}.{constraint_name}"))?;
            ensure!(
                row.get::<_, bool>(8)
                    && row.get::<_, bool>(9)
                    && !row.get::<_, bool>(10)
                    && !row.get::<_, bool>(11),
                "unsatisfied overlay schema requirement: {schema}.{name}.{constraint_name} must be enforced, validated, and immediate"
            );
            let local: Vec<String> = row.get(1);
            let observed = match row.get::<_, &str>(0) {
                "p" => Constraint::primary_key(constraint_name, local)?,
                "u" => Constraint::unique(constraint_name, local)?,
                "c" => Constraint::check(constraint_name, row.get::<_, String>(7))?,
                "f" => {
                    let referenced: Vec<String> = row.get(4);
                    ensure!(
                        local.len() == referenced.len(),
                        "foreign key column pairing differs"
                    );
                    Constraint::foreign_key(
                        constraint_name,
                        local
                            .into_iter()
                            .zip(referenced)
                            .map(|(local, remote)| ForeignKeyColumn::new(local, remote))
                            .collect(),
                        row.get::<_, String>(2),
                        row.get::<_, String>(3),
                        foreign_key_action(row.get(5))?,
                        foreign_key_action(row.get(6))?,
                    )?
                }
                kind => anyhow::bail!(
                    "unsatisfied overlay schema requirement: {schema}.{name}.{constraint_name} has kind {kind}"
                ),
            };
            constraints.push(serde_json::to_value(observed)?);
        }
        tables
            .push(json!({"schema":schema,"name":name,"columns":columns,"constraints":constraints}));
    }
    Ok(json!({"tables":tables}))
}

fn foreign_key_action(action: &str) -> anyhow::Result<ForeignKeyAction> {
    match action {
        "a" => Ok(ForeignKeyAction::NoAction),
        "r" => Ok(ForeignKeyAction::Restrict),
        "c" => Ok(ForeignKeyAction::Cascade),
        "n" => Ok(ForeignKeyAction::SetNull),
        "d" => Ok(ForeignKeyAction::SetDefault),
        other => anyhow::bail!("unsupported observed foreign key action {other}"),
    }
}

fn required_contract_observation(required: &Value, observed: &Value) -> anyhow::Result<Value> {
    let tables = required["tables"]
        .as_array()
        .context("overlay package contract must carry required tables")?;
    ensure!(
        !tables.is_empty(),
        "overlay schema observation must execute at least one requirement"
    );
    let live = observed["tables"]
        .as_array()
        .context("live catalog must carry tables")?;
    let mut field_count = 0;
    let mut constraint_count = 0;
    for table in tables {
        let schema = table["schema"]
            .as_str()
            .context("required schema must be a name")?;
        let name = table["table"]
            .as_str()
            .context("required table must be a name")?;
        let actual = live
            .iter()
            .find(|item| item["schema"] == schema && item["name"] == name)
            .with_context(|| format!("unsatisfied overlay schema requirement: {schema}.{name}"))?;
        let columns = actual["columns"]
            .as_array()
            .context("live relation must carry columns")?;
        for field in table["fields"]
            .as_array()
            .context("required relation must carry fields")?
        {
            let field_name = field["name"]
                .as_str()
                .context("required field must be a name")?;
            let column = columns
                .iter()
                .find(|item| item["name"] == field_name)
                .with_context(|| {
                    format!("unsatisfied overlay schema requirement: {schema}.{name}.{field_name}")
                })?;
            ensure!(
                column["type"] == field["type"] && column["nullable"] == field["nullable"],
                "unsatisfied overlay schema requirement: {schema}.{name}.{field_name} expected type={} nullable={}, observed type={} nullable={}",
                field["type"],
                field["nullable"],
                column["type"],
                column["nullable"]
            );
            field_count += 1;
        }
        let constraints = actual["constraints"]
            .as_array()
            .context("live relation must carry constraints")?;
        for constraint in table["constraints"]
            .as_array()
            .context("required relation must carry constraints")?
        {
            let constraint_name = constraint["name"]
                .as_str()
                .context("required constraint must be a name")?;
            let actual = constraints
                .iter()
                .find(|item| item["name"] == constraint_name)
                .with_context(|| {
                    format!(
                        "unsatisfied overlay schema requirement: {schema}.{name}.{constraint_name}"
                    )
                })?;
            let mut definition = actual
                .as_object()
                .context("live constraint must be an object")?
                .clone();
            definition.remove("name");
            ensure!(
                Value::Object(definition) == constraint["definition"],
                "unsatisfied overlay schema requirement: {schema}.{name}.{constraint_name} definition differs"
            );
            constraint_count += 1;
        }
    }
    ensure!(
        field_count > 0 && constraint_count > 0,
        "overlay schema observation must execute field and constraint requirements"
    );
    Ok(
        json!({"tables":tables.len(),"fields":field_count,"constraints":constraint_count,"result":"satisfied"}),
    )
}

/// The schema the package at `root` needs, as its build wrote it.
fn required_schema_contract(root: &Path) -> anyhow::Result<Value> {
    let relative = Path::new(wamn_schema_generator::REQUIRED_SCHEMA_CONTRACT_PATH)
        .strip_prefix("generated")?;
    let path = wamn_schema_generator::output_root(root).join(relative);
    Ok(serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )?)
}

#[test]
fn required_contract_observation_refuses_changed_consumed_fields_and_constraints() {
    let required = json!({"tables":[{"schema":"receiving","table":"odd_probe",
        "fields":[{"name":"probe_key","type":"uuid","nullable":false}],
        "constraints":[{"name":"odd_probe_key","definition":{"kind":"primary_key","columns":["probe_key"]}}]}]});
    let live = json!({"tables":[{"schema":"receiving","name":"odd_probe",
        "columns":[{"name":"probe_key","type":"uuid","nullable":false},{"name":"unused","type":"text","nullable":true}],
        "constraints":[{"name":"odd_probe_key","kind":"primary_key","columns":["probe_key"]}]}]});
    assert_eq!(
        required_contract_observation(&required, &live).unwrap(),
        json!({"tables":1,"fields":1,"constraints":1,"result":"satisfied"})
    );
    for changed in [json!("text"), json!("int64")] {
        let mut candidate = live.clone();
        candidate["tables"][0]["columns"][0]["type"] = changed;
        let error = required_contract_observation(&required, &candidate).unwrap_err();
        assert!(error.to_string().contains("receiving.odd_probe.probe_key"));
    }
    let mut nullable = live.clone();
    nullable["tables"][0]["columns"][0]["nullable"] = json!(true);
    assert!(
        required_contract_observation(&required, &nullable)
            .unwrap_err()
            .to_string()
            .contains("receiving.odd_probe.probe_key")
    );
    let mut constraint = live;
    constraint["tables"][0]["constraints"][0]["columns"] = json!(["unused"]);
    assert!(
        required_contract_observation(&required, &constraint)
            .unwrap_err()
            .to_string()
            .contains("receiving.odd_probe.odd_probe_key")
    );
}

/// A PostgreSQL 18 server of its own, prepared by deploy/sql/postgres-init.sql,
/// which creates shared roles and the database wamn, applied as the superuser
/// postgres.
async fn production_server() -> anyhow::Result<wamn_test_infrastructure::postgres::OwnedPostgres> {
    let postgres = wamn_test_infrastructure::postgres::start(&[])?;
    let config: tokio_postgres::Config = postgres.database("postgres")?.url().parse()?;
    let password = config
        .get_password()
        .context("the test server URL carries the superuser password")?;
    let init = tokio::process::Command::new("/usr/lib/postgresql/18/bin/psql")
        .env_clear()
        .env("PGHOST", "127.0.0.1")
        .env("PGPORT", config.get_ports()[0].to_string())
        .env("PGUSER", "postgres")
        .env("PGPASSWORD", std::str::from_utf8(password)?)
        .env("PGDATABASE", "postgres")
        .args(["-X", "-q", "-v", "ON_ERROR_STOP=1", "-f"])
        .arg(super::repository_root()?.join("deploy/sql/postgres-init.sql"))
        .output()
        .await
        .context("run psql for deploy/sql/postgres-init.sql")?;
    ensure!(
        init.status.success(),
        "deploy/sql/postgres-init.sql failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    Ok(postgres)
}

/// Install `packages`, bases first, into the new database `name` of `server`
/// through the production boundary, and connect to it.
async fn install_fresh(
    server: &mut wamn_test_infrastructure::postgres::OwnedPostgres,
    name: &str,
    packages: &[std::path::PathBuf],
) -> anyhow::Result<(
    String,
    std::sync::Arc<tokio_postgres::Client>,
    tokio::task::JoinHandle<()>,
)> {
    let url = server.create_database(name)?.url().to_owned();
    let (project, connection_task) = super::connect(&url).await?;
    let fresh = project.query_one("SELECT current_setting('server_version_num')::integer, current_database()::text, to_regnamespace('receiving') IS NULL AND to_regnamespace('catalog') IS NULL", &[]).await?;
    ensure!(
        (180_000..190_000).contains(&fresh.get::<_, i32>(0)) && fresh.get::<_, bool>(2),
        "observer check requires fresh PostgreSQL 18"
    );
    project
        .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
        .await?;
    project
        .batch_execute(&wamn_control_provision::sql::set_database_owner_sql(
            fresh.get(1),
        ))
        .await?;
    super::install_journey_platform_floor(&project, super::PLATFORM_DOMAIN).await?;
    let roots = packages
        .iter()
        .map(|package| {
            Ok(wamn_control::apply_package::PackageRoot {
                path: package.clone(),
                artifact_digest: Some(
                    wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(package)?,
                ),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    for package in &roots {
        apply_package::apply_package(ApplyPackageRequest {
            changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
            package: package.clone(),
            database_url: url.clone(),
            org: identity().org.clone(),
            project: identity().project.clone(),
            environment: identity().environment.clone(),
        })
        .await?;
    }
    wamn_control::reconcile_package_data_access::reconcile_package_data_access(
        wamn_control::reconcile_package_data_access::ReconcilePackageDataAccessRequest {
            packages: roots,
            database_url: url.clone(),
            org: identity().org.clone(),
            project: identity().project.clone(),
            environment: identity().environment.clone(),
        },
    )
    .await?;
    Ok((url, project, connection_task))
}

/// Narrow seam check; the paired journey remains the fresh-install test.
#[tokio::test]
async fn installed_contract_observer_reads_a_production_install_and_refuses_changed_requirements()
-> anyhow::Result<()> {
    let mut postgres = production_server().await?;
    let (_, project, connection_task) = install_fresh(
        &mut postgres,
        "observer",
        &[super::package_root(), super::overlay_package_root()],
    )
    .await?;
    let version: i32 = project
        .query_one("SELECT current_setting('server_version_num')::integer", &[])
        .await?
        .get(0);
    let schema_acl = project
        .query_one(
            "SELECT nspacl::text FROM pg_namespace WHERE nspname = 'receiving'",
            &[],
        )
        .await?
        .get::<_, Option<String>>(0)
        .context("production install must retain an explicit schema ACL")?;
    let required = &required_schema_contract(&super::overlay_package_root())?;
    let baseline = observe_installed_contract(&project, required).await?;
    let positive = required_contract_observation(required, &baseline)?;
    project
        .batch_execute(
            "ALTER TABLE receiving.purchase_order ADD COLUMN overlay_compatibility_note text",
        )
        .await?;
    let additive = observe_installed_contract(&project, required).await?;
    ensure!(
        baseline != additive,
        "all-column projection omitted the additive field"
    );
    ensure!(
        required_contract_observation(required, &additive)? == positive,
        "additive observation changed satisfied requirements"
    );
    let controls = [
        (
            "consumed-nullability",
            "ALTER TABLE receiving.purchase_order ALTER COLUMN status DROP NOT NULL",
            "receiving.purchase_order.status",
        ),
        (
            "consumed-check",
            "ALTER TABLE receiving.quality_inspection DROP CONSTRAINT quality_inspection_status_check; ALTER TABLE receiving.quality_inspection ADD CONSTRAINT quality_inspection_status_check CHECK (status IN ('pending', 'approved', 'rejected'))",
            "receiving.quality_inspection.quality_inspection_status_check",
        ),
        (
            "unvalidated-check",
            "ALTER TABLE receiving.quality_inspection DROP CONSTRAINT quality_inspection_status_check; ALTER TABLE receiving.quality_inspection ADD CONSTRAINT quality_inspection_status_check CHECK (status IN ('pending', 'approved')) NOT VALID",
            "receiving.quality_inspection.quality_inspection_status_check",
        ),
        (
            "deferrable-foreign-key",
            "ALTER TABLE receiving.quality_inspection ALTER CONSTRAINT quality_inspection_receipt_id_fkey DEFERRABLE",
            "receiving.quality_inspection.quality_inspection_receipt_id_fkey",
        ),
    ];
    let mut negative = Vec::new();
    for (name, sql, expected) in controls {
        project.batch_execute("BEGIN").await?;
        project.batch_execute(sql).await?;
        let outcome = match observe_installed_contract(&project, required).await {
            Ok(observed) => required_contract_observation(required, &observed),
            Err(error) => Err(error),
        };
        project.batch_execute("ROLLBACK").await?;
        let error = outcome
            .err()
            .with_context(|| format!("observer accepted {name}"))?;
        ensure!(
            error.to_string().contains(expected),
            "{name} refused for a different reason: {error}"
        );
        negative.push(json!({"control":name,"refusal":error.to_string(),"result":"refused"}));
    }
    let restored = observe_installed_contract(&project, required).await?;
    ensure!(
        restored == additive,
        "rolled-back controls changed the installed projection"
    );
    let retained_acl: String = project
        .query_one(
            "SELECT nspacl::text FROM pg_namespace WHERE nspname = 'receiving'",
            &[],
        )
        .await?
        .get(0);
    ensure!(
        retained_acl == schema_acl,
        "observer changed installed ACLs"
    );
    let evidence = json!({
        "case":"installed-overlay-contract-observer", "server_version_num":version,
        "installed_schema_acl":schema_acl, "retained_schema_acl":retained_acl,
        "schema_admission_claim":false,
        "positive":positive, "controls":negative,
        "baseline_observed_schema_sha256":component_digest(&serde_json::to_vec(&baseline)?),
        "additive_observed_schema_sha256":component_digest(&serde_json::to_vec(&additive)?),
        "result":"pass",
    });
    println!("{}", serde_json::to_string_pretty(&evidence)?);
    drop(project);
    connection_task.await?;
    Ok(())
}

/// An additive base change leaves the overlay unchanged and compatible
/// (wamn-7k2fh, the additive overlay pair that b8199fb4c deleted): a base whose
/// initial migration adds a nullable field to `purchase_order`, rebuilt in a
/// test-owned workspace, installs fresh with the overlay as built, and the
/// overlay's required schema is satisfied exactly as on the unchanged base.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires: cargo, rustc"]
async fn an_additive_base_change_leaves_the_overlay_unchanged_and_compatible() -> anyhow::Result<()>
{
    // The nullable field the additive base adds.
    const FIELD: &str = "overlay_compatibility_note";
    wamn_test_postgres::require_prerequisites(&["cargo", "rustc"]);
    let overlay = super::overlay_package_root();
    let overlay_before =
        wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(&overlay)?;
    let scratch = super::ScratchRoot::create()?;
    let copies = scratch.path().join("copies");
    super::package_copies::create(&copies, &[super::package_root()])?;
    let additive_base = super::package_copies::package(&copies, "wamn_receiving");
    let migration = additive_base.join("migrations/0001_initial.sql");
    let header = "CREATE TABLE receiving.purchase_order (\n";
    let text = std::fs::read_to_string(&migration)?;
    ensure!(
        text.matches(header).count() == 1,
        "the initial migration declares purchase_order once"
    );
    // Apply preflights every mutation before it runs DDL, so the change is to
    // the fresh CREATE, not an ALTER of a relation absent at preflight.
    std::fs::write(
        &migration,
        text.replacen(header, &format!("{header}    {FIELD} text,\n"), 1),
    )?;
    let build_server = wamn_test_postgres::database();
    super::package_copies::build(&copies, &["wamn_receiving"], build_server.url()).await?;

    let required = &required_schema_contract(&overlay)?;
    let mut postgres = production_server().await?;
    let mut observations = Vec::new();
    for (name, base) in [
        ("baseline", super::package_root()),
        ("additive", additive_base),
    ] {
        let (_, project, connection_task) =
            install_fresh(&mut postgres, name, &[base, overlay.clone()])
                .await
                .with_context(|| format!("install the unchanged overlay over the {name} base"))?;
        let observed = observe_installed_contract(&project, required).await;
        drop(project);
        connection_task.await?;
        observations.push(observed?);
    }
    let [baseline, additive] = observations.as_slice() else {
        unreachable!("two installs were observed");
    };
    let field = |observed: &Value| -> anyhow::Result<Option<Value>> {
        Ok(observed["tables"]
            .as_array()
            .context("observed tables are an array")?
            .iter()
            .find(|table| table["schema"] == "receiving" && table["name"] == "purchase_order")
            .context("the install holds purchase_order")?["columns"]
            .as_array()
            .context("observed columns are an array")?
            .iter()
            .find(|column| column["name"] == FIELD)
            .cloned())
    };
    ensure!(
        field(baseline)?.is_none(),
        "the baseline holds the additive field"
    );
    ensure!(
        field(additive)?
            .is_some_and(|column| column["type"] == "text" && column["nullable"] == true),
        "the additive install lacks its nullable text field"
    );
    let satisfied = required_contract_observation(required, baseline)?;
    ensure!(
        required_contract_observation(required, additive)? == satisfied,
        "the additive base changed the overlay's satisfied requirements"
    );
    ensure!(
        wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(&overlay)?
            == overlay_before,
        "the overlay package changed during the additive install"
    );
    println!("ADDITIVE_BASE_OVERLAY result=pass requirements={satisfied}");
    Ok(())
}
