use super::{BTreeMap, PackageManifest, ensure};
use anyhow::Context as _;

/// The sequence defaults of `packages`, each under the application that
/// holds the declaring package in `environment.k`.
pub fn sequence_defaults<'a>(
    applications: &wamn_catalog::EnvironmentApplications,
    packages: impl IntoIterator<Item = &'a PackageManifest>,
) -> anyhow::Result<Vec<wamn_platform_identity::configuration::SequenceDefault>> {
    let mut defaults = BTreeMap::new();
    for package in packages {
        let id = package.package.id.as_str();
        let application =
            wamn_catalog::application_for_package(applications, id).with_context(|| {
                format!("package {id} must belong to exactly one declared application")
            })?;
        for (name, declaration) in &package.sequences {
            let key = (application.to_owned(), name.clone());
            if let Some((other, pattern)) = defaults.get(&key) {
                anyhow::ensure!(
                    pattern == &declaration.pattern,
                    "application {application}: packages {other} and {id} declare sequence {name} with different patterns"
                );
            } else {
                defaults.insert(key, (id, declaration.pattern.clone()));
            }
        }
    }
    Ok(defaults
        .into_iter()
        .map(|((application, name), (_, pattern))| {
            wamn_platform_identity::configuration::SequenceDefault {
                application,
                name,
                pattern,
            }
        })
        .collect())
}

/// The order in which the stage applies the release's `packages`, each an id
/// and the base packages it extends: every package after the release
/// packages it extends.
///
/// # Errors
///
/// When the packages extend one another in a cycle.
pub(super) fn apply_order(packages: &[(&str, Vec<&str>)]) -> anyhow::Result<Vec<usize>> {
    let mut order = Vec::with_capacity(packages.len());
    while order.len() < packages.len() {
        let ready = (0..packages.len())
            .filter(|index| !order.contains(index))
            .filter(|&index| {
                packages[index].1.iter().all(|base| {
                    packages
                        .iter()
                        .position(|(id, _)| id == base)
                        .is_none_or(|base| order.contains(&base))
                })
            })
            .collect::<Vec<_>>();
        ensure!(
            !ready.is_empty(),
            "the release's packages extend one another in a cycle"
        );
        order.extend(ready);
    }
    Ok(order)
}
