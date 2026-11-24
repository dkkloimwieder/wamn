use anyhow::{Context as _, ensure};
use sqlx::Row as _;
use wamn_component_policy::PackageManifest;

/// Install admitted connection requirements from the published package.
pub async fn install(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    manifest: &PackageManifest,
) -> anyhow::Result<u64> {
    if manifest.connections.is_empty() {
        return Ok(0);
    }
    let deploy = manifest
        .deploy
        .as_ref()
        .context("package has no compiled deploy section")?;
    ensure!(
        deploy.declarations.is_empty(),
        "install requires a published package"
    );
    let mut written = 0;
    for component in deploy.components.values() {
        let digest = component.descriptor.component_digest.to_string();
        for connection in &component.descriptor.connections {
            let name = connection
                .store_alias
                .name()
                .context("package connection needs a fixed alias")?;
            let declaration = manifest
                .connections
                .get(name)
                .with_context(|| format!("package declares no connection {name}"))?;
            let mode = match declaration.mode {
                wamn_package::manifest::ConnectionMode::Read => "read",
                wamn_package::manifest::ConnectionMode::Write => "write",
            };
            written += sqlx::query("INSERT INTO catalog.connection_requirement (component_digest,package_id,name,interface,mode) \
                 VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
                .bind(digest.as_str()).bind(manifest.package.id.as_str()).bind(name.as_str()).bind(&declaration.interface).bind(mode)
                .execute(&mut **tx).await?.rows_affected();
            let exact: bool = sqlx::query("SELECT EXISTS (SELECT FROM catalog.connection_requirement WHERE component_digest=$1 \
                 AND package_id=$2 AND name=$3 AND interface=$4 AND mode=$5)")
                .bind(digest.as_str()).bind(manifest.package.id.as_str()).bind(name.as_str()).bind(&declaration.interface).bind(mode)
                .fetch_one(&mut **tx).await?.get(0);
            ensure!(
                exact,
                "package {} connection {name} differs from its installed requirement",
                manifest.package.id
            );
        }
    }
    Ok(written)
}
