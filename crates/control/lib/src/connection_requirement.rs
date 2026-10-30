//! Component imports recorded at package install.

use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_schema_generator::PackageManifest;

/// Read canonical interface identities from the component's import world.
pub(crate) fn imported_interfaces(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    world_interfaces(bytes, false)
}
pub(crate) fn exported_interfaces(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    world_interfaces(bytes, true)
}
fn world_interfaces(bytes: &[u8], exports: bool) -> anyhow::Result<Vec<String>> {
    let wit_component::DecodedWasm::Component(resolve, world) = wit_component::decode(bytes)?
    else {
        anyhow::bail!("connection authority needs a component binary")
    };
    let world = &resolve.worlds[world];
    let items = if exports {
        &world.exports
    } else {
        &world.imports
    };
    let mut names = items
        .values()
        .filter_map(|item| match item {
            wit_parser::WorldItem::Interface { id, .. } => resolve.id_of(*id),
            _ => None,
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    Ok(names)
}
pub(crate) fn require_imports(bytes: &[u8], declared: &[String]) -> anyhow::Result<Vec<String>> {
    let imports = imported_interfaces(bytes)?;
    for interface in declared {
        ensure!(
            imports.contains(interface),
            "declared interface {interface} is not imported by the component"
        );
    }
    Ok(declared.to_vec())
}

pub(crate) async fn install(
    tx: &Transaction<'_>,
    env_key: &str,
    root: &Path,
    manifest: &PackageManifest,
    source: Option<&wamn_runtime::component_artifact_source::ComponentArtifactSourceConfig>,
) -> anyhow::Result<u64> {
    if manifest.connections.is_empty() {
        return Ok(0);
    }
    let mut written = 0;
    for (digest, connections, bytes) in Box::pin(crate::package_artifact::connection_components(
        root, manifest, source,
    ))
    .await?
    {
        ensure!(
            wamn_engine::component_admission::component_digest(&bytes) == digest,
            "component differs from its admitted digest {digest}"
        );
        for name in &connections {
            let declaration = manifest
                .connections
                .get(name)
                .with_context(|| format!("package declares no connection {name}"))?;
            let interfaces = require_imports(&bytes, &declaration.interfaces)?;
            let mode = match declaration.mode {
                wamn_schema_generator::ConnectionMode::Read => "read",
                wamn_schema_generator::ConnectionMode::Write => "write",
            };
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 6] = [
                &env_key,
                &digest,
                &manifest.package.id,
                &name,
                &interfaces,
                &mode,
            ];
            written += tx.execute(
                "INSERT INTO catalog.connection_requirement (env_key,component_digest,package_id,name,interfaces,mode) \
                 VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING", &params,
            ).await?;
            let exact: bool = tx.query_one(
                "SELECT EXISTS (SELECT FROM catalog.connection_requirement WHERE env_key=$1 AND component_digest=$2 \
                 AND package_id=$3 AND name=$4 AND interfaces=$5 AND mode=$6)", &params,
            ).await?.get(0);
            ensure!(
                exact,
                "package {} connection {name} differs from its installed requirement",
                manifest.package.id
            );
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    #[test]
    fn install_refuses_a_declared_interface_not_imported() {
        let mut resolve = test_wit_parser::Resolve::new();
        let package = resolve
            .push_str(
                "imports.wit",
                r"
            package wamn:connection@0.1.0;
            interface http { get: func(); }
            interface metadata { describe: func(); }
            world client { import http; import metadata; export http; }
        ",
            )
            .unwrap();
        let world = resolve.select_world(&[package], Some("client")).unwrap();
        let mut module = test_wit_component::dummy_module(
            &resolve,
            world,
            test_wit_parser::ManglingAndAbi::Standard32,
        );
        test_wit_component::embed_component_metadata(
            &mut module,
            &resolve,
            world,
            test_wit_component::StringEncoding::UTF8,
        )
        .unwrap();
        let bytes = test_wit_component::ComponentEncoder::default()
            .module(&module)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap();
        assert_eq!(
            super::imported_interfaces(&bytes).unwrap(),
            [
                "wamn:connection/http@0.1.0",
                "wamn:connection/metadata@0.1.0",
            ]
        );
        assert_eq!(
            super::exported_interfaces(&bytes).unwrap(),
            ["wamn:connection/http@0.1.0"]
        );
        assert!(
            super::require_imports(&bytes, &["wasmcloud:blobstore/blobstore@0.1.0".into()])
                .is_err()
        );
        super::require_imports(&bytes, &["wamn:connection/http@0.1.0".into()]).unwrap();
    }
}
