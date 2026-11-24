//! Immutable component requirements prepared independently of package SQL.

pub(super) mod plan;

use anyhow::{Context as _, ensure};
use wamn_component_policy::PackageManifest;

/// Select application-owned connections that can supply a platform declaration.
pub fn palette_connections<'a>(
    connection: &wamn_package::ComponentConnection,
    packages: impl IntoIterator<Item = &'a PackageManifest>,
) -> std::collections::BTreeMap<
    (
        &'a wamn_package::PackageName,
        &'a wamn_package::canonical_name::CanonicalName,
    ),
    &'a wamn_package::manifest::ConnectionDeclaration,
> {
    let mut names = std::collections::BTreeMap::new();
    for package in packages {
        for (name, declaration) in &package.connections {
            if connection.store_alias.accepts(name)
                && declaration
                    .interface
                    .split_once('/')
                    .is_some_and(|(import, _)| {
                        import == connection.requirement_type.import_package()
                    })
            {
                names.insert((&package.package.id, name), declaration);
            }
        }
    }
    names
}

#[cfg(test)]
mod palette_tests {
    #[test]
    fn configured_connections_keep_each_application_declaration_owner() {
        let packages: Vec<wamn_component_policy::PackageManifest> = ["orders", "shipping"]
            .into_iter()
            .map(|id| {
                serde_json::from_value(serde_json::json!({
                    "package": {"id": id, "version": "1.0.0"},
                    "required_platform_policy_contract": {"id": "fixture_data_access", "state": "satisfied"},
                    "connections": {"labels": {"interface": "wasmcloud:blobstore/blobstore@0.1.0", "mode": "write"}},
                    "models": {}, "components": {}
                }))
                .unwrap()
            })
            .collect();
        let connection = wamn_package::ComponentConnection {
            store_alias: wamn_package::ComponentConnectionAlias::Configuration {
                parameter: "store_alias".parse().unwrap(),
            },
            requirement_type: wamn_package::ComponentConnectionType::Blobstore,
            mode: wamn_package::ConnectionMode::Write,
        };
        let selected = super::palette_connections(&connection, &packages);
        for package in &packages {
            for (alias, declaration) in &package.connections {
                assert_eq!(
                    selected.get(&(&package.package.id, alias)),
                    Some(&declaration)
                );
            }
        }
    }
}

/// Check that the component's exact named imports match its package declarations.
pub fn validate_connections(
    bytes: &[u8],
    manifest: &PackageManifest,
    connections: &[wamn_package::canonical_name::CanonicalName],
) -> anyhow::Result<()> {
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
        if wamn_package::manifest::host_connection_interface(&interface)
            || interface.starts_with("wamn:postgres/")
        {
            continue;
        }
        ensure!(
            connections.iter().any(|name| name == label)
                && manifest
                    .connections
                    .get(label.as_str())
                    .is_some_and(|declared| declared.interface == interface),
            "connection undeclared: {label} importing {interface}"
        );
    }
    for name in connections {
        let declaration = manifest
            .connections
            .get(name)
            .with_context(|| format!("package declares no connection {name}"))?;
        plan::require_import(&resolve, world, name.as_str(), &declaration.interface)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn install_requires_the_declared_label_for_a_typed_interface() {
        let mut resolve = wit_parser::Resolve::new();
        let package = resolve.push_str("typed.wit", "package example:meter@1.0.0; interface read { get: func() -> u16; } world client { import registers: read; }").unwrap();
        let world = resolve.select_world(&[package], Some("client")).unwrap();
        super::plan::require_import(&resolve, world, "registers", "example:meter/read@1.0.0")
            .unwrap();
        assert!(
            super::plan::require_import(&resolve, world, "other", "example:meter/read@1.0.0")
                .is_err()
        );
        assert!(
            super::plan::require_import(&resolve, world, "bad_name", "example:meter/read@1.0.0")
                .is_err()
        );
        assert!(
            super::plan::require_import(&resolve, world, "registers", "example:meter/write@1.0.0")
                .is_err()
        );
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

        assert!(
            super::plan::require_import(
                &resolve,
                world,
                "files",
                "wasmcloud:blobstore/blobstore@0.1.0"
            )
            .is_err()
        );
        super::plan::require_import(&resolve, world, "upstream", "wamn:connection/http@0.1.0")
            .unwrap();
    }
}
