//! Immutable component requirements prepared independently of package SQL.

use sqlx::Row as _;
pub(super) mod plan;
pub(crate) use plan::exported_interfaces;
pub use plan::require_imports;

use std::path::Path;

use anyhow::{Context as _, ensure};
use wamn_compiler::manifest::PackageManifest;

pub(crate) async fn install(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
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
        written += write(tx, project_env_key, manifest, &digest, &connections, &bytes).await?;
    }
    Ok(written)
}

/// Prepare requirements only for components in the selected release, without installing SQL.
pub async fn prepare_release(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    release: &wamn_catalog::ServingManifest,
    packages: &[PackageManifest],
    source: Option<wamn_runtime::component_artifact_source::ComponentArtifactSourceConfig>,
) -> anyhow::Result<()> {
    use wamn_engine::artifact_source::ArtifactSource as _;
    let source = source
        .map(wamn_runtime::component_artifact_source::ComponentArtifactSource::new)
        .transpose()?;
    for component in &release.components {
        let package = packages
            .iter()
            .find(|package| package.package.id == component.package_id)
            .context("release component has no package manifest")?;
        let names = crate::package_artifact::component_connection_names(
            package,
            &component.descriptor.component,
            &component.descriptor.connections,
        );
        if names.is_empty() {
            continue;
        }
        let source = source
            .as_ref()
            .context("set WAMN_COMPONENT_ARTIFACT_BASE for activation")?;
        let target = wamn_catalog::ComponentPackageCoordinate::new(
            project_env_key,
            &package.package.id,
            &package.package.version,
        )?;
        let admitted = component.descriptor.clone().into_admitted(target);
        let bytes = source.pull_verified(&admitted.component).await?;
        write(
            tx,
            project_env_key,
            package,
            &component.descriptor.component_digest,
            &names,
            &bytes,
        )
        .await?;
    }
    Ok(())
}

/// Insert or reuse exact requirements from digest-verified component bytes.
pub async fn write(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    manifest: &PackageManifest,
    digest: &str,
    connections: &[String],
    bytes: &[u8],
) -> anyhow::Result<u64> {
    let mut written = 0;
    ensure!(
        wamn_digest::bytes_sha256(bytes) == digest,
        "component differs from its admitted digest {digest}"
    );
    let wit_component::DecodedWasm::Component(resolve, world) = wit_component::decode(bytes)?
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
        if wamn_compiler::manifest::host_connection_interface(&interface)
            || interface.starts_with("wamn:postgres/")
        {
            continue;
        }
        ensure!(
            connections.iter().any(|name| name == label)
                && manifest
                    .connections
                    .get(label)
                    .is_some_and(|declared| declared.interface == interface),
            "connection undeclared: {label} importing {interface}"
        );
    }
    for name in connections {
        let declaration = manifest
            .connections
            .get(name)
            .with_context(|| format!("package declares no connection {name}"))?;
        let interface = require_imports(bytes, name, &declaration.interface)?;
        let mode = match declaration.mode {
            wamn_compiler::manifest::ConnectionMode::Read => "read",
            wamn_compiler::manifest::ConnectionMode::Write => "write",
        };
        written += sqlx::query("INSERT INTO catalog.connection_requirement (project_env_key,component_digest,package_id,name,interface,mode) \
             VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING").bind(project_env_key).bind(digest).bind(&manifest.package.id).bind(name).bind(&interface).bind(mode).execute(&mut **tx).await.map(|result| result.rows_affected())?;
        let exact: bool = sqlx::query("SELECT EXISTS (SELECT FROM catalog.connection_requirement WHERE project_env_key=$1 AND component_digest=$2 \
             AND package_id=$3 AND name=$4 AND interface=$5 AND mode=$6)").bind(project_env_key).bind(digest).bind(&manifest.package.id).bind(name).bind(&interface).bind(mode).fetch_one(&mut **tx).await?.get(0);
        ensure!(
            exact,
            "package {} connection {name} differs from its installed requirement",
            manifest.package.id
        );
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
            super::plan::world_interfaces(&bytes, false).unwrap(),
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
