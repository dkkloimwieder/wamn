//! Fresh installations with one immutable Acme overlay.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_ctl::apply_package::{self, ApplyPackageArgs, ApplyPackageError, ApplyPackageErrorKind};
use wamn_runtime::component_admission::component_digest;

use super::{BASE_PACKAGE_ID, JourneyPackage, OVERLAY_COMPONENT, OVERLAY_PACKAGE_ID, TENANT};

const INITIAL_MIGRATION: &str = "migrations/0001_initial.sql";
const ORDER_HEADER: &str = "CREATE TABLE receiving.purchase_order (\n";
const ADDITIVE_FIELD: &str = "overlay_compatibility_note";
const CONFLICT_FIELD: &str = "acme_inspection_required";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum BaseCandidate {
    Baseline,
    Additive,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CompatibilityPhase {
    pub(super) base: BaseCandidate,
    pub(super) package_directory: PathBuf,
    pub(super) evidence_file: PathBuf,
}

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
    let catalog = wamn_schema_generator::introspect_package(project_url, &overlay).await?;
    let observed = serde_json::to_value(&catalog)?;
    let weld: Value =
        serde_json::from_slice(&std::fs::read(overlay.join("generated/package-weld.json"))?)?;
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
    evidence["catalog_sha256"] = json!(component_digest(&catalog.canonical_json_bytes()));
    evidence["overlay_component_sha256"] = json!(digest_file(
        &component_directory.join(format!("{OVERLAY_COMPONENT}.wasm"))
    )?);
    evidence["required_schema_contract"] = weld["required_schema_contract"].clone();
    evidence["schema_admission_claim"] = json!(false);
    write_evidence(phase, &evidence)
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
