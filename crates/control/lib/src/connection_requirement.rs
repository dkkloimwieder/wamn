//! Component imports recorded at package install.

use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_schema_generator::PackageManifest;

/// Read canonical interface identities from the component's import world.
pub(crate) fn interfaces(bytes: &[u8], package: &str) -> anyhow::Result<Vec<String>> {
    let wit_component::DecodedWasm::Component(resolve, world) = wit_component::decode(bytes)?
    else {
        anyhow::bail!("a connection requirement needs a component binary");
    };
    let mut interfaces = Vec::new();
    for item in resolve.worlds[world].imports.values() {
        if let wit_parser::WorldItem::Interface { id, .. } = item
            && let Some(name) = resolve.id_of(*id)
            && name
                .split_once('/')
                .is_some_and(|(import, _)| import == package)
        {
            interfaces.push(name);
        }
    }
    interfaces.sort();
    interfaces.dedup();
    ensure!(
        !interfaces.is_empty(),
        "component imports no interface from {package}"
    );
    Ok(interfaces)
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
        for connection in &connections {
            let name = &connection.store_alias;
            let declaration = manifest
                .connections
                .get(name)
                .with_context(|| format!("package declares no connection {name}"))?;
            let interfaces = interfaces(&bytes, connection.requirement_type.import_package())?;
            let mode = match declaration.mode {
                wamn_schema_generator::ConnectionMode::Read => "read",
                wamn_schema_generator::ConnectionMode::Write => "write",
            };
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 7] = [
                &env_key,
                &digest,
                &manifest.package.id,
                &name,
                &declaration.integration,
                &interfaces,
                &mode,
            ];
            written += tx.execute(
                "INSERT INTO catalog.connection_requirement (env_key,component_digest,package_id,name,integration_type,interfaces,mode) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING", &params,
            ).await?;
            let exact: bool = tx.query_one(
                "SELECT EXISTS (SELECT FROM catalog.connection_requirement WHERE env_key=$1 AND component_digest=$2 \
                 AND package_id=$3 AND name=$4 AND integration_type=$5 AND interfaces=$6 AND mode=$7)", &params,
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
    fn reads_every_imported_interface_of_the_declared_family() {
        let mut resolve = test_wit_parser::Resolve::new();
        let package = resolve
            .push_str(
                "imports.wit",
                r"
            package wamn:connection@0.1.0;
            interface http { get: func(); }
            interface metadata { describe: func(); }
            world client { import http; import metadata; }
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
            super::interfaces(&bytes, "wamn:connection").unwrap(),
            [
                "wamn:connection/http@0.1.0",
                "wamn:connection/metadata@0.1.0",
            ]
        );
        assert!(super::interfaces(&bytes, "wasmcloud:blobstore").is_err());
    }
}
