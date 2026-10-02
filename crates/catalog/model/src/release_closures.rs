//! The permission closures of a serving release (docs/plan/platform-ui.md
//! §2.3 and §2.4). `wamn-ctl` and the application host routes both read
//! them, so a grant writes the same rows wherever it runs.

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

    /// Every `(root, permission)` pair with `permission != root`, as two
    /// parallel arrays.
    pub fn required_pairs(&self) -> (Vec<&str>, Vec<&str>) {
        self.closures
            .iter()
            .flat_map(|(root, closure)| {
                closure
                    .iter()
                    .filter(move |permission| *permission != root)
                    .map(move |permission| (root.as_str(), permission.as_str()))
            })
            .unzip()
    }
}
