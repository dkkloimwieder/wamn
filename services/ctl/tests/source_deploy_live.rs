//! Source preparation is isolated and failures leave authored selections unchanged.

use sqlx::Row as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::ensure;
use serde_json::json;
use wamn_compiler::build_record;
use wamn_environment::environment::document::EnvironmentDocument;

fn package(root: &Path, id: &str, version: &str) -> anyhow::Result<PathBuf> {
    let root = root.join(id);
    std::fs::create_dir_all(root.join("migrations"))?;
    std::fs::write(
        root.join("wamn.k"),
        format!(
            r#"import manifest
manifest.Package {{
    package = {{id = "{id}", version = "{version}"}}
    required_platform_policy_contract = {{id = "source_fixture", state = "satisfied"}}
    components = {{fixture = {{}}}}
    connections = {{}}
    models = {{item = manifest.Model("item", "{id}") {{
        table = "item"
        owner = "{id}"
        audit_log = {{columns = [], retention = "none"}}
        operations: {{get: {{result = "one"}}}}
    }}}}
}}
"#
        ),
    )?;
    std::fs::write(
        root.join("migrations/0001_initial.sql"),
        format!("CREATE TABLE {id}.item (id uuid CONSTRAINT item_id_pkey PRIMARY KEY);"),
    )?;
    Ok(root)
}

#[tokio::test]
async fn source_builds_only_selected_outputs_and_keeps_selection_on_failure() -> anyhow::Result<()>
{
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let database = wamn_test_postgres::database();
    let mut client = wamn_postgres::connect(database.url()).await?;

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE TABLE untouched (value text); INSERT INTO untouched VALUES ('retained');")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)?;
    let auth = scratch.path().join("registry.json");
    std::fs::write(
        &auth,
        serde_json::to_vec(
            &json!({"auths": {"registry.invalid": {"username": "fixture", "password": "fixture"}}}),
        )?,
    )?;
    let file = scratch.path().join("environment.k");
    let document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org": "acme", "project": "source", "env": "dev", "target": "cloud", "policy": "dev","revocation_seconds":60,
        "applications": {
            "a": {"revocation_seconds":5,"hostname": "a.invalid"},
            "b": {"revocation_seconds":5,"hostname": "b.invalid"}
        }
    }))?)?;
    std::fs::write(&file, document.to_kcl())?;
    let deploy = |root: &Path, schema: bool, development: bool| -> anyhow::Result<()> {
        let application = if root.ends_with("appb") { "b" } else { "a" };
        let selected = wamn_environment::environment::analyse::select_application(
            &document,
            Some(application),
        )?;
        std::fs::write(&file, selected.to_kcl())?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_wamn"));
        if development {
            command.arg("dev").arg("--overlay-root");
        } else if schema {
            command.args(["schema", "apply"]);
        } else {
            command.args([
                "deploy",
                "--application",
                if root.ends_with("appb") { "b" } else { "a" },
            ]);
        }
        let output = command
            .arg(root)
            .arg(if development { "--config" } else { "--env" })
            .arg(&file)
            .env("DATABASE_URL", database.url())
            .env("WAMN_CONTROL_URL", "http://127.0.0.1:0")
            .env("WAMN_TOKEN", "caller-session")
            .env_remove("KUBECONFIG")
            .env_remove("WAMN_APPLY_DATABASE_URL")
            .env_remove("WAMN_CONTROL_DATABASE_URL")
            .env_remove("WAMN_SYSTEM_ADMIN_URL")
            .env("WAMN_RELEASE_ARTIFACT_BASE", "registry.invalid/releases")
            .env("WAMN_PACKAGE_ARTIFACT_BASE", "registry.invalid/packages")
            .env(
                "WAMN_COMPONENT_ARTIFACT_BASE",
                "registry.invalid/components",
            )
            .env("WAMN_REGISTRY_AUTH_FILE", &auth)
            .env_remove("WAMN_OCI_CA_PATHS")
            .env_remove("WAMN_RELEASE_HOST_GROUP")
            .output()?;
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        ensure!(
            !output.status.success(),
            "the source fixture has no compiled component to publish"
        );
        Ok(())
    };
    let b = package(scratch.path(), "appb", "1.0.0")?;
    deploy(&b, false, false)?;
    let b_output = wamn_compiler::output_root(&b);
    build_record::read(&b_output)?;
    // B's source is unavailable while A builds.
    std::fs::remove_dir_all(&b)?;
    let a = package(scratch.path(), "appa", "1.0.0")?;
    deploy(&a, false, false)?;
    let output = wamn_compiler::output_root(&a);
    build_record::read(&output)?;
    package(scratch.path(), "appa", "2.0.0")?;
    deploy(&a, false, false)?;
    build_record::read(&b_output)?;
    // Generation succeeds, but publication refuses the absent component.
    build_record::read(&output)?;
    // Source schema apply uses the same builder before publication can fail.
    let schema_version = "3.0.0";
    package(scratch.path(), "appa", schema_version)?;
    deploy(&a, true, false)?;
    let schema_build = build_record::read(&output)?;
    assert_eq!(schema_build.package.version, schema_version);
    let authored = wamn_compiler::compile_manifest(&a)?;
    deploy(&a, false, true)?;
    let first = build_record::read(&output)?;
    assert_ne!(first.package.version, schema_version);
    deploy(&a, false, true)?;
    let repeated = build_record::read(&output)?;
    assert_eq!(first, repeated);
    std::fs::write(a.join("component.rs"), "fn changed_source() {}")?;
    deploy(&a, false, true)?;
    let changed = build_record::read(&output)?;
    assert_ne!(first.package.version, changed.package.version);
    let source = wamn_compiler::manifest::PackageManifest::from_slice(
        &wamn_compiler::compile_manifest(&a)?,
    )?;
    assert_eq!(
        source.package.version,
        wamn_compiler::manifest::PackageManifest::from_slice(&authored)?
            .package
            .version,
    );
    std::fs::write(a.join("migrations/0001_initial.sql"), "INVALID SQL")?;
    deploy(&a, false, false)?;
    assert_eq!(build_record::read(&output)?, changed);
    assert_eq!(
        sqlx::query("SELECT value FROM untouched")
            .fetch_one(&mut client)
            .await?
            .get::<String, _>(0),
        "retained"
    );
    assert!(
        sqlx::query("SELECT to_regnamespace('appa')::text")
            .fetch_one(&mut client)
            .await?
            .get::<Option<String>, _>(0)
            .is_none()
    );
    Ok(())
}
