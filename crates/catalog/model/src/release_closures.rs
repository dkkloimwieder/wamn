//! The permission closures of a serving release (docs/plan/platform-ui.md
//! §2.3 and §2.4). A stored permission is a root. The host builds these
//! closures once from the manifest it loads and expands the caller's roots
//! through them on every request (platform-deploy.md R18).

use std::collections::{BTreeMap, BTreeSet};

use crate::{ServingComponent, ServingManifest, sealed_operation_reference};

/// The permission closure of every operation a serving release registers,
/// keyed by stable reference. Each closure holds the operation itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseClosures {
    closures: BTreeMap<String, BTreeSet<String>>,
}

impl ReleaseClosures {
    /// Read the closures that publish folded into the release components.
    pub fn from_manifest(manifest: &ServingManifest) -> Self {
        Self::from_components(&manifest.components)
    }

    /// Read the closures of `components`. A palette export registers no
    /// operation and contributes none.
    pub fn from_components<'a>(components: impl IntoIterator<Item = &'a ServingComponent>) -> Self {
        let mut closures: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for operation in components
            .into_iter()
            .flat_map(|component| component.operations.values())
        {
            let Some(sealed) = &operation.registered_operation else {
                continue;
            };
            let root = sealed_operation_reference(sealed).to_owned();
            let closure = closures.entry(root.clone()).or_default();
            closure.insert(root);
            closure.extend(
                operation
                    .permissions
                    .iter()
                    .map(|permission| sealed_operation_reference(permission).to_owned()),
            );
        }
        Self { closures }
    }

    /// The closure of `reference`, or `None` when the release does not serve it.
    pub fn closure(&self, reference: &str) -> Option<&BTreeSet<String>> {
        self.closures.get(reference)
    }

    /// Every operation the release registers, by stable reference.
    pub fn roots(&self) -> impl Iterator<Item = &str> {
        self.closures.keys().map(String::as_str)
    }

    /// The union of the closures of `roots`: the effective permissions of a
    /// caller who holds them. A root the release does not serve expands to
    /// nothing.
    pub fn expand<'a>(&self, roots: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
        roots
            .into_iter()
            .filter_map(|root| self.closures.get(root))
            .flatten()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_unions_served_roots_and_drops_unserved_ones() {
        let closures = ReleaseClosures {
            closures: BTreeMap::from([
                (
                    "cat:orders/create".to_owned(),
                    BTreeSet::from([
                        "cat:orders/create".to_owned(),
                        "cat:orders/price".to_owned(),
                    ]),
                ),
                (
                    "cat:orders/list".to_owned(),
                    BTreeSet::from(["cat:orders/list".to_owned()]),
                ),
            ]),
        };
        assert_eq!(
            closures.expand(["cat:orders/create", "cat:orders/list", "cat:orders/gone"]),
            BTreeSet::from([
                "cat:orders/create".to_owned(),
                "cat:orders/list".to_owned(),
                "cat:orders/price".to_owned(),
            ])
        );
        assert!(closures.expand(["cat:orders/gone"]).is_empty());
    }
}
