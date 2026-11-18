use super::{
    Authorities, EnvironmentDocument, EnvironmentRow, Platform,
    Value,
};

pub(super) fn replica_refusal(connection: &str, replicas: Option<u64>) -> Option<String> {
    (replicas != Some(1)).then(|| {
        format!(
            "connection {connection} is exclusive on a cloud target: \
             WAMN_RELEASE_HOST_GROUP must name replicas: 1"
        )
    })
}


/// The project database URL: the system URL with the environment's database.
pub(crate) async fn project_url(
    platform: &Platform,
    document: &EnvironmentDocument,
    row: &EnvironmentRow,
) -> anyhow::Result<String> {
    if wamn_postgres::configuration(&platform.system_database_url)?
        .trim_start()
        .starts_with('{')
    {
        return wamn_platform_install::environment::connection(
            &platform.system_database_url,
            &document.triple(),
        )
        .await?
        .to_json();
    }
    wamn_postgres::database_configuration(
        &platform.system_database_url,
        &wamn_provision::project_env_database_name(
            &document.org,
            &document.project,
            &document.env,
            &row.instance_suffix,
        ),
    )
}

/// The newest entry of `helm history` with status `deployed` or `superseded`.
pub(crate) fn newest_successful(history: &[Value]) -> Option<&Value> {
    history
        .iter()
        .filter(|entry| matches!(entry["status"].as_str(), Some("deployed" | "superseded")))
        .max_by_key(|entry| entry["revision"].as_u64())
}

/// Every reason the document is refused against the authorities (§10.1 step 3).
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
                            wamn_catalog::RequirementType::Http,
                            wamn_catalog::ComponentConnectionType::Http
                        ) | (
                            wamn_catalog::RequirementType::Blobstore,
                            wamn_catalog::ComponentConnectionType::Blobstore
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
