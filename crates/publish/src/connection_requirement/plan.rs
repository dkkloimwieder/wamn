use super::ensure;

pub(super) fn require_import(
    resolve: &wit_parser::Resolve,
    world: wit_parser::WorldId,
    name: &str,
    interface: &str,
) -> anyhow::Result<()> {
    ensure!(
        wamn_package::manifest::valid_connection_name(name),
        "connection name must match ^[a-z][a-z0-9-]{{0,62}}$: {name}"
    );
    let host = wamn_package::manifest::host_connection_interface(interface);
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
    Ok(())
}
