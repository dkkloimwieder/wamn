//! The fixed routes that the host runs itself (docs/plan/platform-ui.md §4.2).
//!
//! A host route has the operation contract, request envelope, result shape,
//! limits and CSRF treatment of any route, but the host serves it with a
//! fixed handler instead of a guest component. The platform builds the routes
//! of each set, so a route carries its operation reference with no package
//! version. A serving manifest records only which sets it serves.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{AttachmentType, OperationType, PAT_AUTHENTICATION_MODE, SESSION_AUTHENTICATION_MODE};

/// The package of every host route. No application package has this id.
pub const HOST_ROUTE_PACKAGE: &str = "wamn_control";

/// The path prefix of every host route. Publish refuses an authored route
/// under it.
pub const HOST_ROUTE_PATH_PREFIX: &str = "/wamn_control/";

/// One fixed contract of host routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HostRouteSet {
    /// The application administration contract, served under every
    /// application audience against that environment's database.
    #[serde(rename = "wamn_control:application")]
    Application,
    /// The org and project administration contract of the control serving
    /// root.
    #[serde(rename = "wamn_control:control")]
    Control,
}

/// The host code that serves one host route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostHandler {
    /// The caller's effective permission set in this application.
    PermissionMine,
    /// The caller's current control authority in the token's org.
    ControlMine,
}

/// Who may call one host route, beside a valid credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRouteAuthority {
    /// Every authenticated caller.
    Member,
    /// A holder of the application role `admin`.
    Admin,
}

/// One fixed host route.
#[derive(Debug)]
pub struct HostRoute {
    pub set: HostRouteSet,
    /// The interface and operation, such as `application/permission.mine`.
    pub operation: &'static str,
    pub type_: OperationType,
    pub authority: HostRouteAuthority,
    pub handler: HostHandler,
}

const ROUTES: &[HostRoute] = &[
    HostRoute {
        set: HostRouteSet::Application,
        operation: "application/permission.mine",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Member,
        handler: HostHandler::PermissionMine,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "control/control.mine",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Member,
        handler: HostHandler::ControlMine,
    },
];

/// One host route as an attachment of the route table.
#[derive(Debug)]
pub struct HostAttachment {
    pub route: &'static HostRoute,
    /// `wamn_control:<interface>/<operation>`, with no package version.
    pub reference: String,
    pub definition: Value,
    pub auth_policy: Value,
}

impl HostAttachment {
    pub fn kind(&self) -> AttachmentType {
        AttachmentType::Http
    }

    /// The operation an `admin`-only route requires. A member route
    /// requires none.
    pub fn registered_operation(&self) -> Option<&str> {
        (self.route.authority == HostRouteAuthority::Admin).then_some(self.reference.as_str())
    }
}

static ATTACHMENTS: LazyLock<BTreeMap<String, HostAttachment>> = LazyLock::new(|| {
    ROUTES
        .iter()
        .map(|route| {
            let id = format!("wamn-control-{}", route.operation.replace(['/', '.'], "-"));
            let method = if route.type_.is_read() { "GET" } else { "POST" };
            let modes = match route.set {
                HostRouteSet::Application => {
                    json!([PAT_AUTHENTICATION_MODE, SESSION_AUTHENTICATION_MODE])
                }
                // A control audience is for browser sessions only.
                HostRouteSet::Control => json!([SESSION_AUTHENTICATION_MODE]),
            };
            let attachment = HostAttachment {
                route,
                reference: format!("{HOST_ROUTE_PACKAGE}:{}", route.operation),
                definition: json!({
                    "id": id,
                    "type": "http",
                    "route": {
                        "host": "*",
                        "method": method,
                        "path": format!("{HOST_ROUTE_PATH_PREFIX}{}", route.operation),
                    },
                }),
                auth_policy: json!({ "modes": modes }),
            };
            (id, attachment)
        })
        .collect()
});

impl HostRouteSet {
    /// Every route of this set, by attachment id.
    pub fn attachments(self) -> impl Iterator<Item = (&'static str, &'static HostAttachment)> {
        ATTACHMENTS
            .iter()
            .filter(move |(_, attachment)| attachment.route.set == self)
            .map(|(id, attachment)| (id.as_str(), attachment))
    }

    /// One route of this set by attachment id.
    pub fn attachment(self, id: &str) -> Option<&'static HostAttachment> {
        ATTACHMENTS
            .get(id)
            .filter(|attachment| attachment.route.set == self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_routes_carry_versionless_references_and_their_set_policy() {
        let application = HostRouteSet::Application
            .attachment("wamn-control-application-permission-mine")
            .expect("permission.mine is an application host route");
        assert_eq!(
            application.reference,
            "wamn_control:application/permission.mine"
        );
        assert_eq!(application.registered_operation(), None);
        assert_eq!(
            application.definition["route"]["path"],
            "/wamn_control/application/permission.mine"
        );
        assert_eq!(
            application.auth_policy,
            json!({"modes": ["pat", "session"]})
        );

        let control = HostRouteSet::Control
            .attachment("wamn-control-control-control-mine")
            .expect("control.mine is a control host route");
        assert_eq!(control.auth_policy, json!({"modes": ["session"]}));
        assert!(
            HostRouteSet::Application
                .attachment("wamn-control-control-control-mine")
                .is_none(),
            "an application release never serves a control route"
        );
    }
}
