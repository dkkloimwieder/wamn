use super::{
    Authorities, BTreeSet, EnvironmentDocument, EnvironmentRow, Notice, Observation, Platform,
    Revision, Value,
};
use anyhow::Context as _;

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
    packages: &[wamn_catalog::manifest::PackageManifest],
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
    let mut url = url::Url::parse(&platform.system_database_url).context("parse the system URL")?;
    url.set_path(&format!(
        "/{}",
        wamn_control_provision::project_env_database_name(
            &document.org,
            &document.project,
            &document.env,
            &row.instance_suffix
        )
    ));
    Ok(url.to_string())
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
        refusals.extend(application_refusals(
            &document.membership(),
            release
                .manifest
                .release
                .packages
                .iter()
                .map(wamn_catalog::PackageCoordinate::package_id),
        ));
    }
    refusals
}

/// The refusals of the declared applications against the packages of the
/// release (docs/architecture/execution.md#workflows): each package of the
/// release belongs to exactly one application, and each assigned package is in
/// the release.
pub fn application_refusals<'a>(
    applications: &wamn_catalog::EnvironmentApplications,
    release_packages: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let release_packages: BTreeSet<&str> = release_packages.into_iter().collect();
    let mut refusals = Vec::new();
    for package in &release_packages {
        let owners: Vec<&str> = applications
            .iter()
            .filter(|(_, application)| application.packages.contains(*package))
            .map(|(name, _)| name.as_str())
            .collect();
        match owners.as_slice() {
            [] => refusals.push(format!(
                "package {package} of the release is assigned to no application"
            )),
            [_] => {}
            _ => refusals.push(format!(
                "package {package} is assigned to the applications {}",
                owners.join(", ")
            )),
        }
    }
    for (name, application) in applications {
        for package in &application.packages {
            if !release_packages.contains(package.as_str()) {
                refusals.push(format!(
                    "application {name} names package {package}, which the release does not carry"
                ));
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
    fingerprints: &std::collections::BTreeMap<String, String>,
) -> Vec<Notice> {
    let mut notices = Vec::new();
    for (package, selected) in fingerprints {
        let installed = authorities
            .project
            .as_ref()
            .and_then(|project| project.installed.get(package));
        if installed.map(|installed| &installed.physical_schema_fingerprint) != Some(selected) {
            notices.push(Notice {
                application: application.to_owned(),
                identity: format!(
                    "{}/{}/{}/catalog.packages/{package}",
                    document.org, document.project, document.env
                ),
                observed: Observation::SchemaDifference {
                    installed: installed
                        .map(|installed| installed.physical_schema_fingerprint.clone()),
                    selected: selected.clone(),
                },
                next_action: format!("wamn schema diff {application} --env {}", document.env),
            });
        }
    }
    if let Some(difference) = authorities
        .project
        .as_ref()
        .and_then(|project| project.schema_difference.as_ref())
    {
        notices.push(Notice {
            application: application.to_owned(),
            identity: format!(
                "{}/{}/{}/platform.schema_baseline",
                document.org, document.project, document.env
            ),
            observed: Observation::SchemaDifference {
                installed: difference.installed.clone(),
                selected: difference.candidate.clone(),
            },
            next_action: format!(
                "Inspect the project SQL baseline for {}/{}/{}",
                document.org, document.project, document.env
            ),
        });
    }
    if let Some(release) = &authorities.release {
        let cause = match &release.qualification {
            Ok(Some(_)) => return notices,
            Ok(None) => None,
            Err(cause) => Some(cause.clone()),
        };
        notices.push(Notice {
            application: application.to_owned(),
            identity: release.digest.clone(),
            observed: Observation::NotChecked {
                cause,
                image_set: std::collections::BTreeMap::from([
                    ("host".into(), release.image_set.host.clone()),
                    ("http".into(), release.image_set.http.clone()),
                    (
                        "materializer".into(),
                        release.image_set.materializer.clone(),
                    ),
                ]),
            },
            next_action: format!(
                "Run optional qualification for release {} on the selected image set",
                release.digest
            ),
        });
    }
    notices
}
