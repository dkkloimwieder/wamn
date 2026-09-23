//! Generate real exclusion diagnostics and run the fixture error-mapping test.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_schema_generator::{
    AuthoredSql, GenerationInput, GenerationProvenance, StatementTransactionality, generate,
    introspect_package,
};
use wamn_test_infrastructure::{postgres, scratch::ScratchRoot};

fn sql_paths(value: &Value, paths: &mut BTreeSet<String>) {
    match value {
        Value::String(path)
            if Path::new(path)
                .extension()
                .is_some_and(|extension| extension == "sql") =>
        {
            paths.insert(path.clone());
        }
        Value::Array(values) => {
            for value in values {
                sql_paths(value, paths);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                sql_paths(value, paths);
            }
        }
        _ => {}
    }
}

async fn generate_fixture(url: &str, package: &Path) -> anyhow::Result<()> {
    let manifest_path = package.join("wamn.json");
    let manifest_bytes = std::fs::read(&manifest_path)?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    let catalog = introspect_package(url, package).await?;
    let mut paths = BTreeSet::new();
    sql_paths(&manifest, &mut paths);
    let sources = paths
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(package.join(&path))?;
            Ok((path, bytes))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let authored = sources
        .iter()
        .map(|(path, bytes)| AuthoredSql::new(path, bytes))
        .collect::<Vec<_>>();
    let generated = generate(&GenerationInput::new(
        &catalog,
        &manifest_bytes,
        &authored,
        GenerationProvenance::new("exclusion-test", "repository-toolchain"),
        &StatementTransactionality::unclassified(),
    ))?;
    for file in generated.files() {
        let path = package.join(file.path());
        std::fs::create_dir_all(path.parent().context("generated file parent")?)?;
        std::fs::write(path, file.bytes())?;
    }
    Ok(())
}

async fn diagnostic(client: &tokio_postgres::Client, update: &str) -> anyhow::Result<Value> {
    client.batch_execute("SET ROLE wamn_app").await?;
    let result = client.batch_execute(update).await;
    client.batch_execute("RESET ROLE").await?;
    let error = result.expect_err("the fixture update must violate its exclusion");
    let error = error
        .as_db_error()
        .context("expected PostgreSQL diagnostics")?;
    ensure!(
        error.code().code() == "23P01",
        "expected exclusion violation"
    );
    Ok(
        json!({"sqlstate": error.code().code(), "constraint": error.constraint().context("server constraint name")?}),
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let scratch = ScratchRoot::create()?;
    // Copy tracked source bytes, including current edits, without build output.
    let listed = Command::new("git")
        .current_dir(&root)
        .args([
            "ls-files",
            "-z",
            "apps",
            "crates/platform/runtime/wit/deps/wamn-postgres",
            "rust-toolchain.toml",
        ])
        .output()?;
    ensure!(listed.status.success(), "list application fixture files");
    for name in listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name)?;
        let destination = scratch.path().join(name);
        std::fs::create_dir_all(destination.parent().context("fixture file parent")?)?;
        std::fs::copy(root.join(name), destination)?;
    }
    let mut server = postgres::start(&[])?;
    let database = server.create_database("exclusion_diagnostics")?;
    let (client, connection) =
        tokio_postgres::connect(database.url(), tokio_postgres::NoTls).await?;
    let task = tokio::spawn(connection);
    client.batch_execute("CREATE SCHEMA inventory; CREATE EXTENSION btree_gist; CREATE ROLE wamn_app; GRANT USAGE ON SCHEMA inventory TO wamn_app;").await?;
    client
        .batch_execute(include_str!(
            "../../../apps/platform_fixture/migrations/0001_initial.sql"
        ))
        .await?;
    client.batch_execute("GRANT SELECT, UPDATE ON inventory.widget TO wamn_app;
        INSERT INTO inventory.widget_maker (id, name)
        VALUES ('00000000-0000-4000-8000-000000000f01', 'first maker'),
               ('00000000-0000-4000-8000-000000000f02', 'second maker');
        INSERT INTO inventory.widget (code, maker_id)
        VALUES ('priority', '00000000-0000-4000-8000-000000000f01'),
               ('standard', '00000000-0000-4000-8000-000000000f02');
        ALTER TABLE inventory.widget ADD CONSTRAINT widget_maker_id_excl EXCLUDE USING gist (maker_id WITH =);").await?;
    let fixture = diagnostic(&client, "UPDATE inventory.widget SET maker_id = (SELECT maker_id FROM inventory.widget WHERE code = 'priority') WHERE code = 'standard'").await?;
    generate_fixture(
        database.url(),
        &scratch.path().join("apps/platform_fixture"),
    )
    .await?;
    let diagnostics = scratch.path().join("exclusion-diagnostics.json");
    std::fs::write(
        &diagnostics,
        serde_json::to_vec(&json!({"fixture": fixture}))?,
    )?;
    drop(client);
    task.await??;
    server.stop()?;
    let status = Command::new(root.join("tools/require-test-result"))
        .current_dir(scratch.path())
        .env("WAMN_EXCLUSION_DIAGNOSTICS", &diagnostics)
        .env("CARGO_TARGET_DIR", scratch.path().join("target"))
        .args([
            "cargo",
            "test",
            "--manifest-path",
            "apps/Cargo.toml",
            "--locked",
            "--offline",
            "-p",
            "wamn-platform-fixture-data-access",
            "--lib",
            "error::tests::generated_update_exclusion_from_postgres",
            "--",
            "--exact",
            "--ignored",
        ])
        .status()?;
    ensure!(
        status.success(),
        "generated exclusion test failed for the fixture"
    );
    Ok(())
}
