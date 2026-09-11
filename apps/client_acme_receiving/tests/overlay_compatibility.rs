//! Fresh installations with one immutable Acme overlay.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_ctl::apply_package::{self, ApplyPackageArgs, ApplyPackageError, ApplyPackageErrorKind};
use wamn_runtime::component_admission::component_digest;
use wamn_gate_harness::journey::{BaseCandidate, CompatibilityPhase};
use wamn_schema_introspection::ir::{
    Constraint, ForeignKeyAction, ForeignKeyColumn, postgres_type,
};

use super::{BASE_PACKAGE_ID, JourneyPackage, OVERLAY_COMPONENT, OVERLAY_PACKAGE_ID, TENANT};

const INITIAL_MIGRATION: &str = "migrations/0001_initial.sql";
const ORDER_HEADER: &str = "CREATE TABLE receiving.purchase_order (\n";
const ADDITIVE_FIELD: &str = "overlay_compatibility_note";
const CONFLICT_FIELD: &str = "acme_inspection_required";

pub(super) fn source(phase: &CompatibilityPhase, package: JourneyPackage) -> PathBuf {
    if package.id == BASE_PACKAGE_ID {
        phase.package_directory.join("receiving")
    } else {
        super::overlay_package_root()
    }
}

pub(super) fn prepare(phase: &CompatibilityPhase) -> anyhow::Result<()> {
    ensure!(
        phase.package_directory.is_absolute(),
        "compatibility package directory must be absolute"
    );
    std::fs::create_dir(&phase.package_directory)
        .context("create fresh compatibility package directory")?;
    let base = phase.package_directory.join("receiving");
    super::copy_fresh_only_package(&super::package_root(), &base)?;
    if matches!(phase.base, BaseCandidate::Additive) {
        add_initial_field(&base, &format!("{ADDITIVE_FIELD} text"))?;
    }
    let evidence = json!({
        "case":"unchanged-overlay-compatibility", "invariant":"OVL-SCHEMA", "stage":"prepared", "base":phase.base,
        "base_initial_migration_sha256":digest_file(&base.join(INITIAL_MIGRATION))?,
        "overlay_files":tree_digests(&super::overlay_package_root())?,
    });
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&phase.evidence_file)
        .context("create compatibility evidence without replacing an earlier run")?;
    serde_json::to_writer_pretty(&mut file, &evidence)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn add_initial_field(base: &Path, field: &str) -> anyhow::Result<()> {
    let path = base.join(INITIAL_MIGRATION);
    let migration = std::fs::read_to_string(&path)?;
    ensure!(
        migration.matches(ORDER_HEADER).count() == 1,
        "initial migration must declare purchase_order exactly once"
    );
    // Apply preflights all mutations before it executes any DDL. The candidate
    // changes its fresh CREATE, not an ALTER against a relation absent at preflight.
    std::fs::write(
        path,
        migration.replacen(ORDER_HEADER, &format!("{ORDER_HEADER}    {field},\n"), 1),
    )?;
    Ok(())
}

pub(super) async fn after_install(
    phase: &CompatibilityPhase,
    project_url: &str,
    component_directory: &Path,
) -> anyhow::Result<()> {
    let overlay = super::overlay_package_root();
    let weld: Value =
        serde_json::from_slice(&std::fs::read(overlay.join("generated/package-weld.json"))?)?;
    let (project, connection_task) = super::connect(project_url).await?;
    let observation = observe_installed_contract(&project, &weld["required_schema_contract"]).await;
    drop(project);
    connection_task
        .await
        .context("join installed schema observer")?;
    let observed = observation?;
    let requirements = required_contract_observation(&weld["required_schema_contract"], &observed)?;
    let purchase_order = observed["tables"]
        .as_array()
        .context("catalog tables must be an array")?
        .iter()
        .find(|table| table["schema"] == "receiving" && table["name"] == "purchase_order")
        .context("fresh installation must contain purchase_order")?;
    let additive = purchase_order["columns"]
        .as_array()
        .context("catalog columns must be an array")?
        .iter()
        .find(|field| field["name"] == ADDITIVE_FIELD);
    match phase.base {
        BaseCandidate::Baseline => ensure!(
            additive.is_none(),
            "baseline unexpectedly contains the candidate field"
        ),
        BaseCandidate::Additive => ensure!(
            additive.is_some_and(|field| field["type"] == "text" && field["nullable"] == true),
            "additive fresh installation lacks its nullable text field"
        ),
    }
    let mut evidence = read_evidence(phase)?;
    ensure_unchanged_overlay(&evidence)?;
    evidence["stage"] = json!("installed-schema-observed");
    evidence["schema_observation"] = requirements;
    evidence["observed_schema_sha256"] = json!(component_digest(&serde_json::to_vec(&observed)?));
    evidence["overlay_component_sha256"] = json!(digest_file(
        &component_directory.join(format!("{OVERLAY_COMPONENT}.wasm"))
    )?);
    evidence["required_schema_contract"] = weld["required_schema_contract"].clone();
    evidence["schema_admission_claim"] = json!(false);
    write_evidence(phase, &evidence)
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
        .context("overlay weld must carry required tables")?;
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

/// Prove the unchanged overlay refuses a conflicting base in another empty database.
pub(super) async fn breaking_refusal(
    phase: &CompatibilityPhase,
    system_url: &str,
    component_directory: &Path,
) -> anyhow::Result<()> {
    let root = phase.package_directory.join("breaking");
    std::fs::create_dir(&root).context("create fresh breaking candidate source directory")?;
    let base = root.join("receiving");
    super::copy_fresh_only_package(&super::package_root(), &base)?;
    add_initial_field(
        &base,
        &format!("{CONFLICT_FIELD} boolean NOT NULL DEFAULT false"),
    )?;
    let database = format!("receiving_overlay_break_{}", uuid::Uuid::new_v4().simple());
    let mut url = reqwest::Url::parse(system_url).context("parse owned PostgreSQL server URL")?;
    url.set_path(&format!("/{database}"));
    url.set_query(None);
    url.set_fragment(None);
    let (admin, admin_task) = super::connect(system_url).await?;
    admin
        .batch_execute(&format!(
            "CREATE DATABASE \"{database}\" OWNER wamn_db_owner TEMPLATE template0"
        ))
        .await
        .context("create separate empty database for breaking candidate")?;
    let proof = breaking_install(&base, url.as_str()).await;
    let cleanup = admin
        .batch_execute(&format!("DROP DATABASE \"{database}\""))
        .await
        .context("remove the exact breaking candidate database");
    let absent = admin
        .query_one(
            "SELECT NOT EXISTS (SELECT FROM pg_database WHERE datname = $1)",
            &[&database],
        )
        .await
        .context("observe breaking candidate database cleanup");
    drop(admin);
    admin_task.abort();
    cleanup?;
    ensure!(
        absent?.get::<_, bool>(0),
        "breaking candidate database remains after cleanup"
    );
    let refusal = proof?;
    let mut evidence = read_evidence(phase)?;
    ensure_unchanged_overlay(&evidence)?;
    ensure!(
        evidence["overlay_component_sha256"]
            == digest_file(&component_directory.join(format!("{OVERLAY_COMPONENT}.wasm")))?,
        "overlay component changed between compatibility and breaking installations"
    );
    evidence["breaking"] = json!({
        "database":database,"base_initial_migration_sha256":digest_file(&base.join(INITIAL_MIGRATION))?,
        "refusal":refusal,"cleanup":"database-absent","overlay_unchanged":true,
    });
    write_evidence(phase, &evidence)
}

async fn breaking_install(base: &Path, database_url: &str) -> anyhow::Result<Value> {
    let (project, connection_task) = super::connect(database_url).await?;
    let result = async {
        project.batch_execute("SET statement_timeout = '20s'; SET lock_timeout = '10s'").await?;
        super::install_journey_platform_floor(project.as_ref()).await?;
        let apply = |package: PathBuf| apply_package::run(ApplyPackageArgs {
            package,database_url:database_url.to_owned(),tenant:TENANT.to_owned(),
        });
        apply(base.to_owned()).await.context("apply the breaking base through the production boundary")?;
        let before = project.query_one("SELECT count(*) FROM catalog.packages WHERE tenant_id = $1", &[&TENANT]).await?.get::<_, i64>(0);
        let error = apply(super::overlay_package_root()).await.err().context("unchanged overlay must refuse a base-owned field collision")?;
        let refusal = error.downcast_ref::<ApplyPackageError>().context("breaking combination must produce a typed package refusal")?;
        ensure!(refusal.kind() == ApplyPackageErrorKind::BaseDefinitionMutation
            && refusal.schema() == Some("receiving") && refusal.relation() == Some("purchase_order")
            && refusal.definition() == Some(CONFLICT_FIELD) && refusal.owner_package() == Some(BASE_PACKAGE_ID),
            "breaking combination refused for a different reason: {refusal}");
        let after = project.query_one(
            "SELECT (SELECT count(*) FROM catalog.packages WHERE tenant_id = $1), \
             (SELECT count(*) FROM catalog.packages WHERE tenant_id = $1 AND package_id = $2), \
             to_regclass('receiving.quality_inspection') IS NULL, \
             NOT EXISTS (SELECT FROM information_schema.columns WHERE table_schema = 'receiving' \
             AND table_name = 'purchase_order' AND column_name = 'acme_quality_status')",
            &[&TENANT, &OVERLAY_PACKAGE_ID]).await?;
        ensure!(before == 1 && after.get::<_, i64>(0) == before && after.get::<_, i64>(1) == 0
            && after.get::<_, bool>(2) && after.get::<_, bool>(3),
            "refused overlay left package metadata or partial business schema");
        Ok(json!({"code":refusal.kind().as_str(),"schema":refusal.schema(),"relation":refusal.relation(),
            "definition":refusal.definition(),"owner":refusal.owner_package(),"partial_overlay_state":false}))
    }.await;
    drop(project);
    connection_task
        .await
        .context("join breaking database observer")?;
    result
}

fn digest_file(path: &Path) -> anyhow::Result<String> {
    Ok(component_digest(&std::fs::read(path).with_context(
        || format!("read artifact {}", path.display()),
    )?))
}

fn tree_digests(root: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    fn visit(
        root: &Path,
        directory: &Path,
        entries: &mut BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(root, &entry.path(), entries)?;
            } else {
                ensure!(kind.is_file(), "overlay artifacts must be regular files");
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)?
                    .to_str()
                    .context("overlay artifact path must be UTF-8")?
                    .to_owned();
                entries.insert(relative, digest_file(&path)?);
            }
        }
        Ok(())
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries)?;
    ensure!(
        !entries.is_empty(),
        "overlay artifact identity must include files"
    );
    Ok(entries)
}

fn read_evidence(phase: &CompatibilityPhase) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(&std::fs::read(
        &phase.evidence_file,
    )?)?)
}

fn ensure_unchanged_overlay(evidence: &Value) -> anyhow::Result<()> {
    ensure!(
        evidence["overlay_files"]
            == serde_json::to_value(tree_digests(&super::overlay_package_root())?)?,
        "overlay input or generated artifact changed during compatibility proof"
    );
    Ok(())
}

fn write_evidence(phase: &CompatibilityPhase, evidence: &Value) -> anyhow::Result<()> {
    std::fs::write(&phase.evidence_file, serde_json::to_vec_pretty(evidence)?)?;
    Ok(())
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

/// Narrow seam check; the paired journey remains the fresh-install proof.
#[tokio::test]
#[ignore = "requires a fresh disposable PostgreSQL 18 server and retained evidence path"]
async fn installed_contract_observer_preserves_acls_and_refuses_changed_requirements()
-> anyhow::Result<()> {
    use wamn_schema_introspection::postgres::{PostgresIntrospectionErrorKind, read_catalog};

    let url = std::env::var("WAMN_OVERLAY_OBSERVER_DATABASE_URL")?;
    let evidence_path = std::env::var("WAMN_OVERLAY_OBSERVER_EVIDENCE_FILE")?;
    let (project, connection_task) = super::connect(&url).await?;
    let server = project.query_one("SELECT current_setting('server_version_num')::integer, current_database()::text, to_regnamespace('receiving') IS NULL AND to_regnamespace('catalog') IS NULL", &[]).await?;
    let version: i32 = server.get(0);
    ensure!(
        (180_000..190_000).contains(&version) && server.get::<_, bool>(2),
        "observer check requires fresh PostgreSQL 18"
    );
    project
        .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
        .await?;
    project
        .batch_execute(&wamn_control_provision::sql::set_database_owner_sql(
            server.get(1),
        ))
        .await?;
    super::install_journey_platform_floor(&project).await?;
    for package in [super::package_root(), super::overlay_package_root()] {
        apply_package::run(ApplyPackageArgs {
            package,
            database_url: url.clone(),
            tenant: TENANT.to_owned(),
        })
        .await?;
    }
    wamn_ctl::reconcile_package_data_access::run(
        wamn_ctl::reconcile_package_data_access::ReconcilePackageDataAccessArgs {
            packages: vec![super::package_root(), super::overlay_package_root()],
            database_url: url.clone(),
            tenant: TENANT.to_owned(),
        },
    )
    .await?;
    let schema_acl = project
        .query_one(
            "SELECT nspacl::text FROM pg_namespace WHERE nspname = 'receiving'",
            &[],
        )
        .await?
        .get::<_, Option<String>>(0)
        .context("production install must retain an explicit schema ACL")?;
    let refusal = read_catalog(&project, &["receiving"]).await.unwrap_err();
    ensure!(
        refusal.kind() == PostgresIntrospectionErrorKind::UnsupportedAcl,
        "authoring ACL refusal changed: {refusal}"
    );
    let weld: Value = serde_json::from_slice(&std::fs::read(
        super::overlay_package_root().join("generated/package-weld.json"),
    )?)?;
    let required = &weld["required_schema_contract"];
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
        "authoring_refusal":refusal.to_string(), "schema_admission_claim":false,
        "positive":positive, "controls":negative,
        "baseline_observed_schema_sha256":component_digest(&serde_json::to_vec(&baseline)?),
        "additive_observed_schema_sha256":component_digest(&serde_json::to_vec(&additive)?),
        "result":"pass",
    });
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(evidence_path)?;
    serde_json::to_writer_pretty(&mut file, &evidence)?;
    file.write_all(b"\n")?;
    drop(project);
    connection_task.await?;
    Ok(())
}
