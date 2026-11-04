//! Component imports recorded at package install.

use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_catalog::manifest::PackageManifest;

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
pub fn require_imports(bytes: &[u8], name: &str, interface: &str) -> anyhow::Result<String> {
    ensure!(
        wamn_catalog::manifest::valid_connection_name(name),
        "connection name must match ^[a-z][a-z0-9-]{{0,62}}$: {name}"
    );
    let wit_component::DecodedWasm::Component(resolve, world) = wit_component::decode(bytes)?
    else {
        anyhow::bail!("connection authority needs a component binary")
    };
    let host = wamn_catalog::manifest::host_connection_interface(interface);
    let found = resolve.worlds[world].imports.iter().any(|(key, item)| {
        let wit_parser::WorldItem::Interface { id, .. } = item else {
            return false;
        };
        let matches = resolve.id_of(*id).as_deref() == Some(interface);
        matches && (host || matches!(key, wit_parser::WorldKey::Name(label) if label == name))
    });
    ensure!(
        found,
        "declared connection {name} does not import {interface} under its required label"
    );
    Ok(interface.to_owned())
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
        let wit_component::DecodedWasm::Component(resolve, world) = wit_component::decode(&bytes)?
        else {
            anyhow::bail!("connection authority needs a component binary")
        };
        for (key, item) in &resolve.worlds[world].imports {
            let (wit_parser::WorldKey::Name(label), wit_parser::WorldItem::Interface { id, .. }) =
                (key, item)
            else {
                continue;
            };
            let Some(interface) = resolve.id_of(*id) else {
                continue;
            };
            if wamn_catalog::manifest::host_connection_interface(&interface)
                || interface.starts_with("wamn:postgres/")
            {
                continue;
            }
            ensure!(
                connections.contains(label)
                    && manifest
                        .connections
                        .get(label)
                        .is_some_and(|declared| declared.interface == interface),
                "connection undeclared: {label} importing {interface}"
            );
        }
        for name in &connections {
            let declaration = manifest
                .connections
                .get(name)
                .with_context(|| format!("package declares no connection {name}"))?;
            let interface = require_imports(&bytes, name, &declaration.interface)?;
            let mode = match declaration.mode {
                wamn_catalog::manifest::ConnectionMode::Read => "read",
                wamn_catalog::manifest::ConnectionMode::Write => "write",
            };
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 6] = [
                &env_key,
                &digest,
                &manifest.package.id,
                &name,
                &interface,
                &mode,
            ];
            written += tx.execute(
                "INSERT INTO catalog.connection_requirement (env_key,component_digest,package_id,name,interface,mode) \
                 VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING", &params,
            ).await?;
            let exact: bool = tx.query_one(
                "SELECT EXISTS (SELECT FROM catalog.connection_requirement WHERE env_key=$1 AND component_digest=$2 \
                 AND package_id=$3 AND name=$4 AND interface=$5 AND mode=$6)", &params,
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
    fn install_requires_the_declared_label_for_a_typed_interface() {
        let mut resolve = wit_parser::Resolve::new();
        let package = resolve.push_str("typed.wit", "package example:meter@1.0.0; interface read { get: func() -> u16; } world client { import registers: read; }").unwrap();
        let world = resolve.select_world(&[package], Some("client")).unwrap();
        let mut module =
            wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
        wit_component::embed_component_metadata(
            &mut module,
            &resolve,
            world,
            wit_component::StringEncoding::UTF8,
        )
        .unwrap();
        let bytes = wit_component::ComponentEncoder::default()
            .module(&module)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap();
        super::require_imports(&bytes, "registers", "example:meter/read@1.0.0").unwrap();
        assert!(super::require_imports(&bytes, "other", "example:meter/read@1.0.0").is_err());
        assert!(super::require_imports(&bytes, "bad_name", "example:meter/read@1.0.0").is_err());
        assert!(super::require_imports(&bytes, "registers", "example:meter/write@1.0.0").is_err());
    }

    #[test]
    fn install_refuses_a_declared_interface_not_imported() {
        let mut resolve = wit_parser::Resolve::new();
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
        let mut module =
            wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
        wit_component::embed_component_metadata(
            &mut module,
            &resolve,
            world,
            wit_component::StringEncoding::UTF8,
        )
        .unwrap();
        let bytes = wit_component::ComponentEncoder::default()
            .module(&module)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap();
        assert_eq!(
            super::world_interfaces(&bytes, false).unwrap(),
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
            super::require_imports(&bytes, "files", "wasmcloud:blobstore/blobstore@0.1.0").is_err()
        );
        super::require_imports(&bytes, "upstream", "wamn:connection/http@0.1.0").unwrap();
    }
}
