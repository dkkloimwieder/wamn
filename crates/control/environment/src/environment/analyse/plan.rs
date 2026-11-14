use super::{
    Authorities, EnvironmentDocument, EnvironmentRow, Notice, Observation, Platform, Revision,
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

/// The refusals of the sequences that the packages of one application declare
/// (docs/architecture/data-access.md#counters): two packages of one application
/// that declare one sequence name declare one default pattern. This reads only
/// the declarations, so a `number_sequence` row whose pattern the admin edited
/// never refuses.
pub(super) fn sequence_refusals(
    applications: &wamn_catalog::EnvironmentApplications,
    packages: &[wamn_compiler::manifest::PackageManifest],
) -> Vec<String> {
    match super::super::stage::sequence_defaults(applications, packages) {
        Ok(_) => Vec::new(),
        Err(error) => vec![error.to_string()],
    }
}

/// The project database URL: the system URL with the environment's database.
pub(crate) fn project_url(
    platform: &Platform,
    document: &EnvironmentDocument,
    row: &EnvironmentRow,
) -> anyhow::Result<String> {
    wamn_postgres::database_configuration(
        &platform.system_database_url,
        &wamn_control_provision::project_env_database_name(
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
        for component in &release.manifest.components {
            for requirement in &component.descriptor.connections {
                if let Some(connection) = authorities.connections.get(&super::super::bindings::key(
                    document
                        .application_for_package(&component.package_id)
                        .unwrap_or(""),
                    &requirement.store_alias,
                )) && !matches!(
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
                        "connection {} has a different type from its release requirement",
                        requirement.store_alias
                    ));
                }
            }
        }
        for component in &release.manifest.components {
            let application = document
                .application_for_package(&component.package_id)
                .unwrap_or("");
            for requirement in &component.descriptor.connections {
                let key = super::super::bindings::key(application, &requirement.store_alias);
                if !authorities.connections.contains_key(&key)
                    && !authorities.artifact_bindings.contains(&key)
                {
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

/// The differences from each authority (§10.1 step 3, the plan).
pub(super) fn plan(document: &EnvironmentDocument, authorities: &Authorities) -> Vec<String> {
    let mut plan = Vec::new();
    for (application, definition) in &document.applications {
        for (name, integration) in &definition.connections {
            plan.push(format!(
                "bind {application}.{name} to integration {integration}"
            ));
        }
    }
    let current = authorities
        .revision
        .as_ref()
        .and_then(Revision::manifest_digest);
    match (&authorities.release, current) {
        (Some(release), Some(current)) if release.digest == current => {
            plan.push(format!("reapply release {current} with Helm"));
        }
        (Some(release), _) => plan.push(format!("stage and roll out release {}", release.digest)),
        (None, Some(current)) => plan.push(format!("uninstall release {current} and drain")),
        (None, None) => {}
    }
    if plan.is_empty() {
        plan.push("nothing differs".to_owned());
    }
    plan
}

/// Compare physical SQL identities and optional evidence, never package versions.
pub(crate) fn notices(
    document: &EnvironmentDocument,
    application: &str,
    authorities: &Authorities,
) -> Vec<Notice> {
    let mut notices = Vec::new();
    if let Some(report) = authorities
        .project
        .as_ref()
        .and_then(|project| project.schema_report.as_ref())
        .filter(|report| !report.warnings.is_empty())
    {
        notices.push(Notice {
            application: application.to_owned(),
            identity: format!(
                "{}/{}/{}/platform._sqlx_migrations",
                document.org, document.project, document.env
            ),
            observed: Observation::PlatformSchema { report: report.clone() },
            next_action: format!(
                "Inspect project SQL with wamn platform schema diff --target project --env FILE --chart DIR for {}/{}/{}",
                document.org, document.project, document.env
            ),
        });
    }
    notices
}
