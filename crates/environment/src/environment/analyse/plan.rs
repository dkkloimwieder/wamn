use super::{Authorities, EnvironmentDocument};
use wamn_registry::EnvironmentRow;

pub(super) fn replica_refusal(connection: &str, replicas: Option<u64>) -> Option<String> {
    (replicas != Some(1)).then(|| {
        format!(
            "connection {connection} is exclusive on a cloud target: \
             WAMN_RELEASE_HOST_GROUP must name replicas: 1"
        )
    })
}


/// Resolve the project endpoint from the generation read during analysis.
pub(crate) fn project_url(
    document: &EnvironmentDocument,
    row: &EnvironmentRow,
) -> anyhow::Result<String> {
    wamn_platform_install::environment::connection_for(&document.triple(), &row.instance_suffix)
        .to_json()
}

pub fn refusals(document: &EnvironmentDocument, authorities: &Authorities) -> Vec<String> {
    let mut refusals = Vec::new();
    if let Some(release) = &authorities.release {
        for package in release.packages.values() {
            let application = document.application_for_package(&package.package.id);
            for requirement in package
                .deploy
                .iter()
                .flat_map(|deploy| deploy.components.values())
                .flat_map(|component| &component.descriptor.connections)
            {
                let Some(alias) = requirement.store_alias.name() else {
                    continue;
                };
                let Some(application) = application else {
                    refusals.push(format!(
                        "connection_unbound: package {} has no application for {alias}",
                        package.package.id,
                    ));
                    continue;
                };
                let key = super::super::bindings::key(application, alias.as_str());
                if let Some(connection) = authorities.connections.get(&key) {
                    if !matches!(
                        (connection.requirement_type, requirement.requirement_type),
                        (
                            wamn_connection_type::RequirementType::Http,
                            wamn_package::ComponentConnectionType::Http
                        ) | (
                            wamn_connection_type::RequirementType::Blobstore,
                            wamn_package::ComponentConnectionType::Blobstore
                        )
                    ) {
                        refusals.push(format!(
                            "connection {alias} has a different type from its release requirement"
                        ));
                    }
                } else if !authorities.artifact_bindings.contains(&key) {
                    refusals.push(format!(
                        "connection_unbound: {key} is required by release {}",
                        release.digest
                    ));
                }
            }
        }
    }
    refusals
}
