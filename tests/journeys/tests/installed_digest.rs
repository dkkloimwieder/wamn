//! Environment apply derives CDC schemas from the installed package digest.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context as _, ensure};
use serde_json::Value;
use wamn_http_client::HttpClient;
use wamn_test_infrastructure::{containers::start_postgres, scratch::ScratchRoot};

use common::{Environment, Org, run};

const PACKAGE: &str = "platform_fixture";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn env_apply_uses_the_installed_digest_after_a_tag_moves() -> anyhow::Result<()> {
    common::capture(
        "env_apply_uses_the_installed_digest_after_a_tag_moves",
        env_apply_uses_the_installed_digest_after_a_tag_moves_case,
    )
    .await
}

async fn env_apply_uses_the_installed_digest_after_a_tag_moves_case() -> anyhow::Result<()> {
    let org = Org::create().await?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let workspace = repository.join(format!(".wamn-installed-digest-{}", org.name));
    fs::create_dir(&workspace)?;
    let scratch = ScratchRoot(workspace);
    let source = scratch.path().join(PACKAGE);
    let mut copy = std::process::Command::new("cp");
    copy.arg("-R")
        .arg(common::request::fixture(PACKAGE))
        .arg(&source);
    run(copy).await?;
    prepare_guest_workspace(&repository, scratch.path()).await?;

    let manifest = source.join("wamn.k");
    let authored = fs::read_to_string(&manifest)?;
    let declaration = authored
        .lines()
        .find(|line| line.trim_start().starts_with("package = {"))
        .context("fixture has no package declaration")?;
    let (_, version) = declaration
        .split_once("version = \"")
        .context("fixture version")?;
    let (version, _) = version
        .split_once('"')
        .context("fixture version terminator")?;
    // This tag belongs to the test even on a reused registry.
    let version = format!("{version}-cdc.{}", org.name);
    let authored = authored.replacen(
        declaration,
        &format!("    package = {{id = \"{PACKAGE}\", version = \"{version}\"}}"),
        1,
    );
    fs::write(&manifest, &authored)?;

    let environment = org.apply(PACKAGE).await?;
    let describe = start_postgres().await?;
    let a = publish(&environment, &source, describe.admin_url()).await?;
    let schemas_a = schemas(&environment, &a).await?;
    assert!(!schemas_a.is_empty(), "A must declare a publication schema");

    let mut deploy = environment.org.command();
    deploy
        .args(["deploy", "--release", &a, "--application", PACKAGE, "--env"])
        .arg(&environment.file);
    run(deploy).await?;

    fs::write(
        source.join("migrations/0003_digest_schema.sql"),
        include_str!("../fixtures/installed_digest/0003_digest_schema.sql"),
    )?;
    let models = "    models = {\n";
    ensure!(authored.contains(models), "fixture has no models block");
    fs::write(
        &manifest,
        authored.replacen(
            models,
            &format!(
                "{models}{}",
                include_str!("../fixtures/installed_digest/model.k")
            ),
            1,
        ),
    )?;
    let b = publish(&environment, &source, describe.admin_url()).await?;
    drop(describe);
    let schemas_b = schemas(&environment, &b).await?;
    assert!(
        schemas_a.is_subset(&schemas_b) && schemas_a != schemas_b,
        "B must add a publication schema"
    );
    let outputs = &environment.org.cluster.outputs;
    let (package_base, _) = wamn_registry::org_artifact_bases(&outputs.registry);
    let (registry, repository) = package_base.split_once('/').context("package repository")?;
    let tag = format!("{PACKAGE}-{version}");
    let tagged = HttpClient::shared()?
        .request(
            hyper::Method::HEAD,
            format!("http://{registry}/v2/{repository}/manifests/{tag}"),
        )
        .header(
            hyper::header::ACCEPT,
            "application/vnd.oci.image.manifest.v1+json",
        )
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(
        tagged
            .headers()
            .get("docker-content-digest")
            .context("digest of the package tag")?
            .to_str()?,
        b
    );

    let mut project = environment.observe().await?;
    assert_eq!(installed_digest(&mut project.connection).await?, a);
    drop(project);

    let mut apply = environment.org.command();
    apply.args(["env", "apply"]).arg(&environment.file);
    run(apply).await?;

    let mut project = environment.observe().await?;
    assert_eq!(installed_digest(&mut project.connection).await?, a);
    let mut system = environment.org.cluster.observe_system().await?;
    let triple = &environment.triple;
    let (publication, stored): (String, Vec<String>) = sqlx::query_as(
        "SELECT publication, schema FROM registry.event_readers WHERE org = $1 AND project = $2 AND env = $3",
    )
    .bind(triple.org.as_str())
    .bind(triple.project.as_str())
    .bind(triple.env.as_str())
    .fetch_one(&mut system.connection)
    .await?;
    let published: Vec<String> = sqlx::query_scalar(
        "SELECT n.nspname::text FROM pg_publication p
         JOIN pg_publication_namespace pn ON pn.pnpubid = p.oid
         JOIN pg_namespace n ON n.oid = pn.pnnspid
         WHERE p.pubname = $1",
    )
    .bind(&publication)
    .fetch_all(&mut project.connection)
    .await?;
    assert_eq!(published.into_iter().collect::<BTreeSet<_>>(), schemas_a);
    assert_eq!(stored.into_iter().collect::<BTreeSet<_>>(), schemas_a);
    Ok(())
}

async fn prepare_guest_workspace(repository: &Path, workspace: &Path) -> anyhow::Result<()> {
    let apps = repository.join("apps");
    let settings = fs::read_to_string(apps.join("Cargo.toml"))?;
    let (_, inherited) = settings
        .split_once("[workspace.lints.rust]")
        .context("app workspace settings")?;
    let inherited = inherited.replace("path = \"crates/", "path = \"../apps/crates/");
    let manifest = workspace.join("Cargo.toml");
    fs::write(
        &manifest,
        format!(
            "[workspace]\nresolver = \"2\"\nmembers = [\"{PACKAGE}/component\"]\n\n[workspace.lints.rust]{inherited}"
        ),
    )?;
    fs::copy(apps.join("Cargo.lock"), workspace.join("Cargo.lock"))?;
    // Cargo narrows the copied lockfile to the test workspace before wamn builds it.
    let mut cargo = std::process::Command::new("cargo");
    cargo
        .args([
            "metadata",
            "--offline",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(manifest)
        .stdout(Stdio::null());
    run(cargo).await?;
    Ok(())
}

async fn publish(
    environment: &Environment,
    source: &Path,
    describe_url: &str,
) -> anyhow::Result<String> {
    let mut build = environment.org.command();
    build
        .args(["build", "--refresh-describe"])
        .arg(source)
        .env("DATABASE_URL", describe_url);
    run(build).await?;
    let mut publish = environment.org.command();
    let workspace = source.parent().context("fixture workspace")?;
    publish
        .arg("publish")
        .arg(workspace.join("target/wamn").join(PACKAGE));
    Ok(run(publish).await?.trim().to_owned())
}

async fn schemas(environment: &Environment, digest: &str) -> anyhow::Result<BTreeSet<String>> {
    let outputs = &environment.org.cluster.outputs;
    let (package_base, _) = wamn_registry::org_artifact_bases(&outputs.registry);
    let mut inspect = environment.org.command();
    inspect
        .args(["inspect", digest, "--insecure-registry", "--artifact-base"])
        .arg(package_base)
        .arg("--registry-auth-file")
        .arg(&outputs.registry_auth_file);
    let manifest: Value = serde_json::from_str(&run(inspect).await?)?;
    manifest["models"]
        .as_object()
        .context("package models")?
        .values()
        .map(|model| Ok(model["schema"].as_str().context("model schema")?.to_owned()))
        .collect()
}

async fn installed_digest(connection: &mut sqlx::PgConnection) -> anyhow::Result<String> {
    Ok(
        sqlx::query_scalar("SELECT artifact_digest FROM catalog.packages WHERE package_id = $1")
            .bind(PACKAGE)
            .fetch_one(connection)
            .await?,
    )
}
