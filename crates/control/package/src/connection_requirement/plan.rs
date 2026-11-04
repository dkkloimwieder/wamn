use super::ensure;

pub(crate) fn exported_interfaces(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    world_interfaces(bytes, true)
}

pub(super) fn world_interfaces(bytes: &[u8], exports: bool) -> anyhow::Result<Vec<String>> {
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
