//! The fixed routes that the host runs itself (docs/plan/platform-ui.md §4.2).
//!
//! A host route has the operation contract, request envelope, result shape,
//! limits and CSRF treatment of any route, but the host serves it with a
//! fixed handler instead of a guest component. The platform builds the routes
//! of each set, and a serving manifest records only which sets it serves.
//!
//! The control contract authors its package id, its version and its route
//! prefixes in `host_route/wamn.json`, as a package does in its `wamn.json`.
//! Each route derives its operation token, path and attachment id from that
//! file exactly as a generated package operation does.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::route_identity::{
    operation_token, route_attachment_id, route_path, sealed_operation_reference,
};
use crate::{AttachmentType, OperationType, PAT_AUTHENTICATION_MODE, SESSION_AUTHENTICATION_MODE};

/// The control contract's manifest: the one place its version is authored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlContract {
    package: ControlPackage,
    client_package: ControlClientPackage,
    control_client_package: ControlClientPackage,
    routes: ControlRoutes,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlClientPackage {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlPackage {
    id: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlRoutes {
    path_prefix: String,
    id_prefix: String,
}

static CONTRACT: LazyLock<ControlContract> = LazyLock::new(|| {
    serde_json::from_str(include_str!("host_route/wamn.json"))
        .expect("the control contract manifest is valid")
});

/// The package of every host route. No application package has this id.
pub fn host_route_package() -> &'static str {
    &CONTRACT.package.id
}

/// The authored version of the host route contract.
pub fn host_route_version() -> &'static str {
    &CONTRACT.package.version
}

/// Whether a stable operation reference or sealed operation id names a host
/// route. A host route is fixed to `admin` or to every member, so no authored
/// role holds one (docs/plan/platform-ui.md §2.5).
pub fn is_host_route_operation(operation: &str) -> bool {
    static TOKEN_PACKAGE: LazyLock<String> =
        LazyLock::new(|| CONTRACT.package.id.replace('_', "-"));
    operation
        .split_once(':')
        .is_some_and(|(package, _)| package == TOKEN_PACKAGE.as_str())
}

/// The path prefix of every host route, with its closing `/`. Publish
/// refuses an authored route under it.
pub fn host_route_path_prefix() -> &'static str {
    static PREFIX: LazyLock<String> = LazyLock::new(|| format!("{}/", CONTRACT.routes.path_prefix));
    &PREFIX
}

/// One fixed contract of host routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HostRouteSet {
    /// The application administration contract, served under every
    /// application audience against that environment's database.
    #[serde(rename = "application")]
    Application,
    /// The org and project administration contract of the control serving
    /// root.
    #[serde(rename = "control")]
    Control,
}

/// The host code that serves one host route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostHandler {
    /// The caller's effective permission set in this application.
    PermissionMine,
    /// The user rows of the application (docs/plan/platform-ui.md §4.6).
    ApplicationUserList,
    /// The roles of the application.
    RoleList,
    /// Create one empty authored role.
    RoleCreate,
    /// Delete one authored role with its assignments and permissions.
    RoleDelete,
    /// The permission rows of one role, with the roots that require each.
    PermissionList,
    /// Select an operation for a role and write its released closure.
    PermissionGrant,
    /// Remove a role's selection of an operation and its closure rows.
    PermissionRevoke,
    /// Give a role to an application user.
    UserRoleGrant,
    /// Take a role from an application user.
    UserRoleRevoke,
    /// The caller's current control authority in the token's org.
    ControlMine,
    /// The members of the org (docs/plan/platform-ui.md §4.4).
    UserList,
    /// Create or reuse a user, make it a member of the org with the
    /// requested grants, and mail the invitation when it has no password.
    UserInvite,
    /// Make an org membership active.
    UserActivate,
    /// Revoke a member's access in the org from the leaves upward.
    UserDeactivate,
    /// The projects of the org.
    ProjectList,
    /// Grant `org-admin` and its project roles and memberships.
    OrgAdminGrant,
    /// Revoke `org-admin` and `project-admin` throughout the org.
    OrgAdminRevoke,
    /// The environments of one project (docs/plan/platform-ui.md §4.5).
    EnvironmentList,
    /// The users with a membership or `project-admin` in one project.
    MemberList,
    /// Grant a membership of one environment to an active org member.
    MemberGrant,
    /// Revoke a membership of one environment.
    MemberRevoke,
    /// Grant `project-admin` and a membership of every environment of the
    /// project.
    ProjectAdminGrant,
    /// Revoke `project-admin` in one project.
    ProjectAdminRevoke,
    /// Make one environment active (docs/plan/platform-ui.md §5.4).
    EnvironmentActivate,
    /// Make one environment inactive.
    EnvironmentInactivate,
    /// Make every environment of one project active.
    ProjectActivate,
    /// Make every environment of one project inactive.
    ProjectInactivate,
}

/// Who may call one host route, beside a valid credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRouteAuthority {
    /// Every authenticated caller. On the control set, that is a holder of
    /// `org-admin` in the token's org or of `project-admin` in one of its
    /// projects, and the handler checks it again.
    Member,
    /// A holder of the application role `admin`.
    Admin,
    /// A holder of `org-admin` in the token's org. The handler checks the
    /// role, again in its write transaction.
    OrgAdmin,
    /// A holder of `org-admin` in the token's org, or of `project-admin` in
    /// the project that the request names. The handler checks the role,
    /// again in its write transaction.
    ProjectAdmin,
}

/// One fixed host route.
#[derive(Debug)]
pub struct HostRoute {
    pub set: HostRouteSet,
    /// The model and action, such as `permission.mine`, as a package
    /// operation names them.
    pub operation: &'static str,
    pub type_: OperationType,
    pub authority: HostRouteAuthority,
    pub handler: HostHandler,
}

const ROUTES: &[HostRoute] = &[
    HostRoute {
        set: HostRouteSet::Application,
        operation: "permission.mine",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Member,
        handler: HostHandler::PermissionMine,
    },
    HostRoute {
        set: HostRouteSet::Application,
        operation: "user.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::ApplicationUserList,
    },
    HostRoute {
        set: HostRouteSet::Application,
        operation: "role.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::RoleList,
    },
    HostRoute {
        set: HostRouteSet::Application,
        operation: "role.create",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::RoleCreate,
    },
    HostRoute {
        set: HostRouteSet::Application,
        operation: "role.delete",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::RoleDelete,
    },
    HostRoute {
        set: HostRouteSet::Application,
        operation: "permission.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::PermissionList,
    },
    HostRoute {
        set: HostRouteSet::Application,
        operation: "permission.grant",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::PermissionGrant,
    },
    HostRoute {
        set: HostRouteSet::Application,
        operation: "permission.revoke",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::PermissionRevoke,
    },
    HostRoute {
        set: HostRouteSet::Application,
        operation: "user_role.grant",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::UserRoleGrant,
    },
    HostRoute {
        set: HostRouteSet::Application,
        operation: "user_role.revoke",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::UserRoleRevoke,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "control.mine",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Member,
        handler: HostHandler::ControlMine,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "user.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Member,
        handler: HostHandler::UserList,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "user.invite",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::UserInvite,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "user.activate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::UserActivate,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "user.deactivate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::UserDeactivate,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::ProjectList,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "org_admin.grant",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::OrgAdminGrant,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "org_admin.revoke",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::OrgAdminRevoke,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "environment.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::EnvironmentList,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "member.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::MemberList,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "member.grant",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::MemberGrant,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "member.revoke",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::MemberRevoke,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project_admin.grant",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::ProjectAdminGrant,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project_admin.revoke",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::ProjectAdminRevoke,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "environment.activate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::EnvironmentActivate,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "environment.inactivate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::EnvironmentInactivate,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project.activate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::ProjectActivate,
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project.inactivate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::ProjectInactivate,
    },
];

/// One host route as an attachment of the route table.
#[derive(Debug)]
pub struct HostAttachment {
    pub route: &'static HostRoute,
    /// The sealed operation id, such as `wamn-control:permission/mine@0.3.0`.
    /// A host-run write stamps it.
    pub operation: String,
    /// The operation id without its version, as a stored permission names it.
    pub reference: String,
    pub definition: Value,
    pub auth_policy: Value,
}

impl HostAttachment {
    pub fn attachment_type(&self) -> AttachmentType {
        AttachmentType::Http
    }

    /// The operation an `admin`-only route requires. A member route
    /// requires none.
    pub fn registered_operation(&self) -> Option<&str> {
        (self.route.authority == HostRouteAuthority::Admin).then_some(self.operation.as_str())
    }
}

/// Every host route by its set and attachment id. The two sets are served
/// under different audiences, so a route of each set can derive one id, as
/// `user.list` does.
static ATTACHMENTS: LazyLock<BTreeMap<(HostRouteSet, String), HostAttachment>> =
    LazyLock::new(|| {
        ROUTES
            .iter()
            .map(|route| {
                let (model, action) = route
                    .operation
                    .split_once('.')
                    .expect("a host route names its model and action");
                let id = route_attachment_id(&CONTRACT.routes.id_prefix, model, action);
                let operation = operation_token(
                    &CONTRACT.package.id,
                    &CONTRACT.package.version,
                    model,
                    action,
                );
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
                    reference: sealed_operation_reference(&operation).to_owned(),
                    operation,
                    definition: json!({
                        "id": id,
                        "type": "http",
                        "route": {
                            "host": "*",
                            "method": method,
                            "path": route_path(&CONTRACT.routes.path_prefix, model, action),
                        },
                    }),
                    auth_policy: json!({ "modes": modes }),
                };
                ((route.set, id), attachment)
            })
            .collect()
    });

impl HostRouteSet {
    /// Every route of this set, by attachment id.
    pub fn attachments(self) -> impl Iterator<Item = (&'static str, &'static HostAttachment)> {
        ATTACHMENTS
            .iter()
            .filter(move |((set, _), _)| *set == self)
            .map(|((_, id), attachment)| (id.as_str(), attachment))
    }

    /// One route of this set by attachment id.
    pub fn attachment(self, id: &str) -> Option<&'static HostAttachment> {
        ATTACHMENTS.get(&(self, id.to_owned()))
    }

    /// The package name of the generated TypeScript client of this set, as
    /// an application's `client_package` names its client. Each set has its
    /// own client, because the two sets share operation ids.
    pub fn client_package(self) -> &'static str {
        match self {
            Self::Application => &CONTRACT.client_package.name,
            Self::Control => &CONTRACT.control_client_package.name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_routes_derive_their_ids_from_the_control_contract() {
        let application = HostRouteSet::Application
            .attachment("wamn-control-permission-mine-http")
            .expect("permission.mine is an application host route");
        assert_eq!(application.operation, "wamn-control:permission/mine@0.3.0");
        assert_eq!(application.reference, "wamn-control:permission/mine");
        assert_eq!(application.registered_operation(), None);
        assert_eq!(
            application.definition["route"]["path"],
            "/wamn_control/permission/mine"
        );
        assert_eq!(
            application.auth_policy,
            json!({"modes": ["pat", "session"]})
        );

        let control = HostRouteSet::Control
            .attachment("wamn-control-control-mine-http")
            .expect("control.mine is a control host route");
        assert_eq!(control.operation, "wamn-control:control/mine@0.3.0");
        assert_eq!(control.auth_policy, json!({"modes": ["session"]}));
        assert!(
            HostRouteSet::Application
                .attachment("wamn-control-control-mine-http")
                .is_none(),
            "an application release never serves a control route"
        );
        assert_eq!(host_route_package(), "wamn_control");
        assert!(is_host_route_operation("wamn-control:role/create"));
        assert!(is_host_route_operation("wamn-control:role/create@0.3.0"));
        assert!(!is_host_route_operation("wamn-receiving:receipt/get"));

        let users = HostRouteSet::Application
            .attachment("wamn-control-user-list-http")
            .expect("user.list is an application host route");
        let members = HostRouteSet::Control
            .attachment("wamn-control-user-list-http")
            .expect("user.list is a control host route");
        assert_eq!(users.route.handler, HostHandler::ApplicationUserList);
        assert_eq!(members.route.handler, HostHandler::UserList);
        assert_eq!(users.operation, members.operation);
        assert_eq!(
            users.registered_operation(),
            Some("wamn-control:user/list@0.3.0")
        );
        assert_eq!(host_route_path_prefix(), "/wamn_control/");
    }
}
