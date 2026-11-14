//! Publication uses a private registry before any destination exists.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, ensure};
use serde_json::json;
use wamn_catalog::{PackageCoordinate, ServingManifest};
use wamn_compiler::build_record::{self, BuildComponent};

#[path = "../../../crates/control/environment/tests/support/local_registry.rs"]
mod local_registry;

fn package(work: &Path, id: &str, version: &str) -> anyhow::Result<PathBuf> {
    let root = work.join("apps").join(id);
    let output = wamn_compiler::output_root(&root);
    std::fs::create_dir_all(output.join("publication"))?;
    std::fs::create_dir_all(root.join("migrations"))?;
    std::fs::write(
        root.join("migrations/0001_initial.sql"),
        format!("CREATE TABLE {id}.item (id uuid CONSTRAINT item_pkey PRIMARY KEY);"),
    )?;
    std::fs::create_dir_all(output.join("components"))?;
    std::fs::write(
        root.join("wamn.k"),
        format!("package = {{id = \"{id}\", version = \"{version}\"}}\n"),
    )?;
    let reference = format!("{id}:item/get");
    let operation = format!("{reference}@{version}");
    let manifest = json!({
        "package": {"id": id, "version": version},
        "required_platform_policy_contract": {"id": "fixture", "state": "satisfied"},
        "models": {"item": {"schema": id, "table": "item", "owner": id,
            "server_owned_fields": [], "audit_log": {"columns": [], "retention": "none"},
            "operations": {"get": {"permission": "item.get", "result": "one"}}}},
        "connections": {}, "components": {"entry": {"connections": ["postgres"]}},
        "compiled_operations": {"item.get": {"operation": {
            "operation": operation, "type": "get", "permission_token": "item.get", "grant": null,
            "relations": [{"schema": id, "table": "item"}], "statements": []
        }}}
    });
    std::fs::write(output.join("wamn.json"), serde_json::to_vec(&manifest)?)?;
    std::fs::write(
        output.join("publication/components.json"),
        serde_json::to_vec(&json!({"entry": {
            "target": {"package-id": id, "package-version": version},
            "component": "entry", "interface-version": "0.1.0",
            "operations": {&operation: {"registered-operation": operation,
                "input-ports": [], "output-ports": [], "parameters": []}}, "connections": []
        }}))?,
    )?;
    let definition = json!({"id": "get", "type": "http", "route": {"path": "/item"}});
    std::fs::write(
        output.join("publication/attachments.json"),
        serde_json::to_vec(&json!({
            "get": {"type": "http", "package-id": id, "component": "entry", "operation": operation,
                "registered-operation": operation, "auth-policy": {"modes": ["pat"]},
                "definition-hash": wamn_digest::canonical_json_sha256(&definition),
                "definition": definition}
        }))?,
    )?;
    let mut resolve = wit_parser::Resolve::new();
    resolve.push_str(
        "node.wit",
        include_str!("../../../wit/wamn-node/package.wit"),
    )?;
    let wit = resolve.push_str(
        "application.wit",
        &format!(
            "package {id}:item@{version};
         interface get {{
           use wamn:node/types@0.1.0.{{node-context, emission, node-error}};
           run: func(context: node-context, input: string) -> result<emission, node-error>;
         }}
         world application {{ export get; }}"
        ),
    )?;
    let world = resolve.select_world(&[wit], None)?;
    let mut module =
        wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
    wit_component::embed_component_metadata(
        &mut module,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )?;
    let bytes = wit_component::ComponentEncoder::default()
        .module(&module)?
        .validate(true)
        .encode()?;
    let path = "components/entry.wasm";
    std::fs::write(output.join(path), &bytes)?;
    std::fs::create_dir_all(output.join("migrations"))?;
    std::fs::copy(
        root.join("migrations/0001_initial.sql"),
        output.join("migrations/0001_initial.sql"),
    )?;
    wamn_test_infrastructure::fixture_digest::write_build_record(&root);
    let mut record = build_record::read(&output)?;
    record.outputs.components.push(BuildComponent {
        name: "entry".into(),
        path: path.into(),
        sha256: wamn_digest::bytes_sha256(&bytes),
    });
    std::fs::write(
        output.join(build_record::BUILD_RECORD_FILE),
        record.to_bytes(),
    )?;
    Ok(root)
}

fn run(command: &mut Command) -> anyhow::Result<String> {
    let output = command.output()?;
    ensure!(
        output.status.success(),
        "wamn-ctl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[tokio::test]
#[ignore = "requires: env:WAMN_FLOW_HTTP_COMPONENT"]
async fn applications_publish_independently_before_destination_creation() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_FLOW_HTTP_COMPONENT");
    let route =
        std::env::var_os("WAMN_FLOW_HTTP_COMPONENT").context("pinned HTTP route component")?;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let registry = local_registry::LocalRegistry::start("application-publication");
    registry.ready().await;
    let packages = format!("{}/packages", registry.address);
    let components = format!("{}/components", registry.address);
    let releases = format!("{}/releases", registry.address);
    let auth = scratch.path().join("config.json");
    std::fs::write(
        &auth,
        serde_json::to_vec(
            &json!({"auths": {&registry.address: {"username": "wamn", "password": "wamn"}}}),
        )?,
    )?;
    let file = scratch.path().join("environment.k");
    let document = wamn_environment_control::environment::document::EnvironmentDocument::from_json(
        &serde_json::to_vec(&json!({
            "org": "unprovisioned", "project": "publication", "env": "dev", "target": "cloud", "policy": "dev","revocation_seconds":60,
            "applications": {
                "a": {"current": "none", "revocation_seconds":5,"hostname": "a.example.invalid", "packages": ["appa"]},
                "b": {"current": "none", "revocation_seconds":5,"hostname": "b.example.invalid", "packages": ["appb"]}
            }
        }))?,
    )?;
    std::fs::write(&file, document.to_kcl())?;
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wamn-ctl"));
        command
            .env("WAMN_RELEASE_ARTIFACT_BASE", &releases)
            .env("WAMN_COMPONENT_ARTIFACT_BASE", &components)
            .env("WAMN_FLOW_HTTP_COMPONENT", &route);
        command
    };
    let push = |root: &Path| -> anyhow::Result<()> {
        run(command()
            .arg("push-package")
            .arg("--package")
            .arg(root)
            .args([
                "--artifact-base",
                &packages,
                "--component-artifact-base",
                &components,
            ])
            .arg("--registry-auth-file")
            .arg(&auth)
            .arg("--insecure-registry"))?;
        Ok(())
    };
    let publish = |application: &str, a_version: &str| {
        run(command()
            .arg("publish-release")
            .arg(&file)
            .args([
                "--application",
                application,
                "--package",
                &format!("appa@{a_version}"),
                "--package",
                "appb@1.0.0",
                "--artifact-base",
                &packages,
            ])
            .arg("--registry-auth-file")
            .arg(&auth)
            .arg("--insecure-registry"))
    };
    push(&package(scratch.path(), "appb", "1.0.0")?)?;
    let source =
        wamn_runtime::release_manifest_source::ReleaseManifestSource::new(&releases, true, &auth)?;
    // A's declared coordinate does not exist yet. B reads only its own closure.
    let b = publish("b", "1.0.0")?;
    let b_bytes = source.pull_verified(&b).await?;
    push(&package(scratch.path(), "appa", "1.0.0")?)?;
    let a = publish("a", "1.0.0")?;
    let a_bytes = source.pull_verified(&a).await?;
    for (id, bytes) in [("appa", &a_bytes), ("appb", &b_bytes)] {
        let (manifest, _) = ServingManifest::from_canonical_bytes(bytes)?;
        assert_eq!(
            manifest.release.packages,
            [PackageCoordinate::new(id, "1.0.0")?].into()
        );
        assert_eq!(
            manifest
                .composed_entry
                .as_ref()
                .unwrap()
                .application
                .package_id,
            id
        );
        assert!(
            manifest
                .components
                .iter()
                .all(|component| component.package_id == id)
        );
        assert!(manifest.routes.iter().all(|route| route.package_id == id));
    }
    assert_eq!(publish("a", "1.0.0")?, a);
    assert_eq!(publish("b", "1.0.0")?, b);
    assert_eq!(source.pull_verified(&a).await?, a_bytes);
    assert_eq!(source.pull_verified(&b).await?, b_bytes);
    push(&package(scratch.path(), "appa", "2.0.0")?)?;
    assert_ne!(publish("a", "2.0.0")?, a);
    assert_eq!(publish("b", "2.0.0")?, b);
    assert_eq!(source.pull_verified(&b).await?, b_bytes);
    Ok(())
}

#[tokio::test]
async fn status_pulls_candidates_with_installed_registry_transport() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let registry = local_registry::LocalRegistry::start("application-status");
    registry.ready().await;
    let packages = format!("{}/packages", registry.address);
    let releases = format!("{}/releases", registry.address);
    let auth = scratch.path().join("config.json");
    std::fs::write(
        &auth,
        serde_json::to_vec(
            &json!({"auths": {&registry.address: {"username": "wamn", "password": "wamn"}}}),
        )?,
    )?;
    let root = package(scratch.path(), "appa", "1.0.0")?;
    run(Command::new(env!("CARGO_BIN_EXE_wamn-ctl"))
        .arg("push-package")
        .arg("--package")
        .arg(&root)
        .args(["--artifact-base", &packages, "--component-artifact-base"])
        .arg(format!("{}/components", registry.address))
        .arg("--registry-auth-file")
        .arg(&auth)
        .arg("--insecure-registry"))?;
    let manifest: ServingManifest = serde_json::from_value(json!({
        "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"packages": [{"package-id": "appa", "package-version": "1.0.0"}]},
        "components": [], "routes": [], "attachments": {}
    }))?;
    let release = wamn_release_control::publish_release::push_manifest_bytes(
        &manifest.canonical_bytes(),
        &releases,
        true,
        &[],
        &auth,
    )
    .await?;
    let document = wamn_environment_control::environment::document::EnvironmentDocument::from_json(
        &serde_json::to_vec(&json!({
            "org": "unprovisioned", "project": "publication", "env": "dev", "target": "cloud", "policy": "dev","revocation_seconds":60,
            "applications": {"a": {"current": release.digest.as_str(),
                "revocation_seconds":5,"hostname": "a.example.invalid", "packages": ["appa"]}}
        }))?,
    )?;
    let file = scratch.path().join("environment.k");
    std::fs::write(&file, document.to_kcl())?;
    // Only the installed control release is available. No routing service or chart exists.
    let bin = scratch.path().join("bin");
    std::fs::create_dir(&bin)?;
    std::fs::write(
        bin.join("environment-configmap.json"),
        serde_json::to_vec(&json!({
            "apiVersion":"v1", "kind":"ConfigMap",
            "metadata":{"name":wamn_control_provision::environment_names::environment_resource_name(
                &document.org, &document.project, &document.env,
            )?, "namespace":"hosts"},
            "data":{"environment.json":serde_json::to_string(&document)?}
        }))?,
    )?;
    for (name, script) in [
        (
            "helm",
            r#"#!/bin/sh
case "$*" in
  *' get values c-unprovisioned -n hosts -o json') cat "$(dirname "$0")/host-values.json" ;;
  history\ *) printf '[]' ;;
  *) exit 1 ;;
esac
"#,
        ),
        (
            "kubectl",
            r#"#!/bin/sh
case "$*" in
  'config current-context --kubeconfig '*) printf 'status-test' ;;
  *'get --raw '*) printf '{"items":[]}' ;;
  *'get configmap/'*) cat "$(dirname "$0")/environment-configmap.json" ;;
  *) exit 1 ;;
esac
"#,
        ),
    ] {
        let path = bin.join(name);
        std::fs::write(&path, script)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    let values = bin.join("host-values.json");
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))?;
    for (configuration, known) in [
        (
            Some(json!({"runtime-operator":{"runtime":{"hostGroups":[{
                "extraArgs":["--allow-insecure-registries"]
            }]}}})),
            true,
        ),
        (
            Some(json!({"runtime-operator":{"runtime":{"hostGroups":[{}]}}})),
            false,
        ),
        (Some(json!({})), false),
        (
            Some(json!({"runtime-operator":{"runtime":{"hostGroups":[{
                "extraArgs":"--allow-insecure-registries"
            }]}}})),
            false,
        ),
        (None, false),
    ] {
        let missing = configuration.is_none();
        match configuration {
            Some(configuration) => std::fs::write(&values, serde_json::to_vec(&configuration)?)?,
            None => std::fs::remove_file(&values)?,
        }
        for (binary, arguments) in [
            (env!("CARGO_BIN_EXE_wamn"), vec!["status", "--env"]),
            (env!("CARGO_BIN_EXE_wamn-ctl"), vec!["env", "status"]),
        ] {
            let status: serde_json::Value = serde_json::from_str(&run(Command::new(binary)
                .args(arguments)
                .arg(&file)
                .env("PATH", &path)
                .env("KUBECONFIG", scratch.path().join("absent-kubeconfig"))
                .env("WAMN_CONTROL_DATABASE_URL", "invalid database URL")
                .env("WAMN_SYSTEM_ADMIN_URL", "invalid database URL")
                .env("WAMN_RELEASE_ARTIFACT_BASE", &releases)
                .env("WAMN_PACKAGE_ARTIFACT_BASE", &packages)
                .env("WAMN_REGISTRY_AUTH_FILE", &auth)
                .env("WAMN_NATS_URL", "tls://absent.invalid:4222")
                .env("WAMN_NATS_TLS_CA", "absent-ca")
                .env("WAMN_NATS_TLS_CERT", "absent-cert")
                .env("WAMN_NATS_TLS_KEY", "absent-key")
                .env_remove("WAMN_RELEASE_HOST_GROUP")
                .env_remove("WAMN_RELEASE_CHART")
                .env_remove("WAMN_OCI_CA_PATHS"))?)?;
            let schema = &status["schema"];
            assert!(schema.is_object());
            assert!(schema["disposable"].is_null());
            let unknowns = schema["unknowns"].as_array().unwrap();
            assert!(
                unknowns
                    .iter()
                    .any(|cause| { cause.as_str().unwrap().contains("database state unknown:") })
            );
            if missing {
                assert!(
                    unknowns.iter().any(|cause| {
                        cause.as_str().unwrap().contains(values.to_str().unwrap())
                    })
                );
            }
            for application in document.applications.keys() {
                assert_eq!(
                    status["applications"][application]["current"],
                    release.digest.as_str()
                );
                assert!(status["applications"][application]["observed"].is_object());
                let candidate = &schema["applications"][application];
                assert_eq!(candidate.is_object(), known, "{status:#}");
                let candidate_unknown = unknowns.iter().any(|cause| {
                    cause
                        .as_str()
                        .unwrap()
                        .contains(&format!("application {application} candidate unknown:"))
                });
                assert_eq!(candidate_unknown, !known);
                if known {
                    assert_eq!(candidate["release"], release.digest.as_str());
                    for package in &manifest.release.packages {
                        assert!(candidate["packages"][package.package_id()].is_object());
                    }
                }
            }
        }
    }
    Ok(())
}
