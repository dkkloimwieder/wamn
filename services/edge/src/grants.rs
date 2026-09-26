//! The grants file: the permissions of each role on the box.
//!
//! Publish writes `grants.json` into the edge release bundle, so the grants are
//! a publish-time fact like the release itself. A verified session token's roles
//! select permissions here until the token expires. The box keeps no session
//! state and reads no user roles.

use std::collections::{BTreeMap, BTreeSet};

use wamn_catalog::ServingManifest;
use wamn_catalog::edge_bundle::{EdgeGrants, GRANTS_FILE_NAME};
use wamn_session::token::is_role_slug;

use crate::release::{EdgeReleaseError, EdgeReleaseErrorKind};

/// The permissions of each role, checked against one release.
#[derive(Clone, Debug)]
pub struct Grants {
    roles: BTreeMap<String, BTreeSet<String>>,
}

impl Grants {
    /// Parse `grants.json` for `manifest`.
    ///
    /// Refuses a role that is not a canonical slug and a permission that no
    /// operation in the release requires.
    pub fn parse(bytes: &[u8], manifest: &ServingManifest) -> Result<Self, EdgeReleaseError> {
        let document: EdgeGrants = serde_json::from_slice(bytes).map_err(|error| {
            rejected(format!(
                "{GRANTS_FILE_NAME} is not a grants document: {error}"
            ))
        })?;
        let required: BTreeSet<&str> = manifest
            .components
            .iter()
            .flat_map(|component| component.operations.values())
            .flat_map(|operation| operation.permissions.iter().map(String::as_str))
            .collect();
        for (role, permissions) in &document.roles {
            if !is_role_slug(role) {
                return Err(rejected(format!("{GRANTS_FILE_NAME} names role {role:?}")));
            }
            if let Some(permission) = permissions
                .iter()
                .find(|permission| !required.contains(permission.as_str()))
            {
                return Err(rejected(format!(
                    "{GRANTS_FILE_NAME} grants {permission} to {role}, and no operation in the \
                     release requires it"
                )));
            }
        }
        Ok(Self {
            roles: document.roles,
        })
    }

    /// The permissions that `roles` hold together. An unknown role holds none.
    pub fn permissions(&self, roles: &[String]) -> BTreeSet<String> {
        roles
            .iter()
            .filter_map(|role| self.roles.get(role))
            .flatten()
            .cloned()
            .collect()
    }
}

fn rejected(detail: String) -> EdgeReleaseError {
    EdgeReleaseError::new(EdgeReleaseErrorKind::Rejected, detail)
}
