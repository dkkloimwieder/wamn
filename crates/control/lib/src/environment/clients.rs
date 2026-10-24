//! Resolve the environment's web client before writing its release chart.

use std::collections::BTreeMap;

use anyhow::{bail, ensure};

use super::document::EnvironmentDocument;

pub(super) fn resolve<'a>(
    document: &EnvironmentDocument,
    packages: impl Iterator<Item = &'a str>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let packages: Vec<_> = packages.collect();
    let selected = match document.client_package.as_deref() {
        Some(name) => {
            ensure!(
                packages.contains(&name),
                "host {} names client package {name}, which the release does not provide",
                document.route_host
            );
            Some(name)
        }
        None => match packages.as_slice() {
            [] => None,
            [only] => Some(*only),
            _ => bail!(
                "hosts [{}] require client_package because the release provides multiple clients: {}",
                document.route_host,
                packages.join(", ")
            ),
        },
    };
    Ok(selected
        .map(|name| (document.route_host.clone(), name.to_owned()))
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> EnvironmentDocument {
        serde_json::from_value(serde_json::json!({
            "org": "test", "project": "wms", "env": "dev", "release": "none",
            "route_host": "wms.example.com", "policy": "dev", "applications": {}
        }))
        .unwrap()
    }

    #[test]
    fn the_only_client_is_the_default() {
        assert_eq!(
            resolve(&document(), ["wms"].into_iter()).unwrap()["wms.example.com"],
            "wms"
        );
        assert!(resolve(&document(), [].into_iter()).unwrap().is_empty());
    }

    #[test]
    fn a_named_client_selects_its_package() {
        let mut document = document();
        document.client_package = Some("receiving".into());
        assert_eq!(
            resolve(&document, ["wms", "receiving"].into_iter()).unwrap()["wms.example.com"],
            "receiving"
        );
        assert!(
            resolve(&document, ["wms"].into_iter())
                .unwrap_err()
                .to_string()
                .contains("receiving")
        );
    }

    #[test]
    fn multiple_clients_without_a_name_refuse_with_the_host() {
        let error = resolve(&document(), ["wms", "receiving"].into_iter()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("hosts [wms.example.com] require client_package")
        );
    }
}
