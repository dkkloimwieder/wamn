//! Source preparation is isolated and failures leave authored selections unchanged.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::ensure;
use serde_json::json;
use wamn_compiler::build_record;
use wamn_environment_control::environment::document::EnvironmentDocument;

fn package(root: &Path, id: &str, version: &str) -> anyhow::Result<PathBuf> {
    let root = root.join(id);
    std::fs::create_dir_all(root.join("migrations"))?;
    std::fs::create_dir_all(root.join("publication"))?;
    std::fs::write(root.join("publication/attachments.json"), "{}")?;
    std::fs::write(
        root.join("wamn.k"),
        format!(
            r#"import manifest
manifest.Package {{
    package = {{id = "{id}", version = "{version}"}}
    required_platform_policy_contract = {{id = "source_fixture", state = "satisfied"}}
    components = {{fixture = {{connections = ["postgres"]}}}}
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
    let (client, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls).await?;
    tokio::spawn(driver);
    client
        .batch_execute(
            "CREATE TABLE untouched (value text); INSERT INTO untouched VALUES ('retained');",
        )
        .await?;
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
            "a": {"current": "none", "revocation_seconds":5,"hostname": "a.invalid", "packages": ["appa"]},
            "b": {"current": "none", "revocation_seconds":5,"hostname": "b.invalid", "packages": ["appb"]}
        }
    }))?)?;
    std::fs::write(&file, document.to_kcl())?;
    let before = std::fs::read(&file)?;
    let deploy = |root: &Path, schema: bool| -> anyhow::Result<()> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wamn"));
        if schema {
            command.args(["schema", "apply"]);
        } else {
            command.arg("deploy");
        }
        let output = command
            .arg(root)
            .arg("--env")
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
        assert_eq!(std::fs::read(&file)?, before);
        Ok(())
    };
    let b = package(scratch.path(), "appb", "1.0.0")?;
    deploy(&b, false)?;
    let b_output = wamn_compiler::output_root(&b);
    let b_record = std::fs::read(b_output.join(build_record::BUILD_RECORD_FILE))?;
    // B's source is unavailable while A builds.
    std::fs::remove_dir_all(&b)?;
    let a = package(scratch.path(), "appa", "1.0.0")?;
    deploy(&a, false)?;
    let output = wamn_compiler::output_root(&a);
    let a_record = std::fs::read(output.join(build_record::BUILD_RECORD_FILE))?;
    package(scratch.path(), "appa", "2.0.0")?;
    deploy(&a, false)?;
    assert_ne!(
        std::fs::read(output.join(build_record::BUILD_RECORD_FILE))?,
        a_record
    );
    assert_eq!(
        std::fs::read(b_output.join(build_record::BUILD_RECORD_FILE))?,
        b_record
    );
    // Generation succeeds, but publication refuses the absent component.
    build_record::read_current(&a, &output)?;
    // Source schema apply uses the same builder before publication can fail.
    let schema_version = "3.0.0";
    package(scratch.path(), "appa", schema_version)?;
    deploy(&a, true)?;
    let schema_build = build_record::read_current(&a, &output)?;
    assert_eq!(schema_build.package.version, schema_version);
    let last_build = std::fs::read(output.join(build_record::BUILD_RECORD_FILE))?;
    std::fs::write(a.join("migrations/0001_initial.sql"), "INVALID SQL")?;
    deploy(&a, false)?;
    assert_eq!(
        std::fs::read(output.join(build_record::BUILD_RECORD_FILE))?,
        last_build
    );
    assert_eq!(
        client
            .query_one("SELECT value FROM untouched", &[])
            .await?
            .get::<_, String>(0),
        "retained"
    );
    assert!(
        client
            .query_one("SELECT to_regnamespace('appa')::text", &[])
            .await?
            .get::<_, Option<String>>(0)
            .is_none()
    );
    Ok(())
}
