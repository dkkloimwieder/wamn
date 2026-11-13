//! Reserved application principals and the built-in role.

/// The fixed WAMN namespace for platform principal ids. Every project and every
/// deployment derives the same id from the same name, so no configuration
/// carries the id. The value is frozen: a change changes every platform row id.
/// Anyone can recompute it from the domain.
///
/// `WAMN_NAMESPACE = uuid5(NAMESPACE_DNS, "wamn.dev")`
pub const WAMN_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0x0df2_99cf_7085_537d_919b_1145_b5ef_00f8);

/// The reserved principal namespace. A platform principal name is
/// `wamn:<component>`, where the component is kebab-case and carries no action
/// and no version. A project or application cannot create a name with this prefix.
pub const PLATFORM_PRINCIPAL_PREFIX: &str = "wamn:";

/// A platform component that writes under its own `platform."user"` row.
///
/// The row names the component, never the invocation. The list is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformComponent {
    /// Project provisioning. Its row is the first row in a project database.
    Provisioning,
    /// Package application, including operation grants.
    ApplyPackage,
    /// Post-commit registration delivery.
    Materializer,
    /// Executor queue delivery and management candidate cases.
    Executor,
    /// The record history retention task, which removes expired log entries.
    AuditRetention,
}

impl PlatformComponent {
    /// Every component. Order is presentational.
    pub const ALL: [PlatformComponent; 5] = [
        PlatformComponent::Provisioning,
        PlatformComponent::ApplyPackage,
        PlatformComponent::Materializer,
        PlatformComponent::Executor,
        PlatformComponent::AuditRetention,
    ];

    /// The kebab-case component name, for example `apply-package`.
    pub fn as_str(self) -> &'static str {
        match self {
            PlatformComponent::Provisioning => "provisioning",
            PlatformComponent::ApplyPackage => "apply-package",
            PlatformComponent::Materializer => "materializer",
            PlatformComponent::Executor => "executor",
            PlatformComponent::AuditRetention => "audit-retention",
        }
    }

    /// The principal name `wamn:<component>`, which `display_name` carries.
    pub fn principal_name(self) -> String {
        format!("{PLATFORM_PRINCIPAL_PREFIX}{}", self.as_str())
    }

    /// The `platform."user"` id: the UUIDv5 of [`Self::principal_name`] under
    /// [`WAMN_NAMESPACE`].
    pub fn principal_id(self) -> uuid::Uuid {
        uuid::Uuid::new_v5(&WAMN_NAMESPACE, self.principal_name().as_bytes())
    }
}

/// The name of the one `service` user that every environment holds for workflow
/// runs (docs/architecture/execution.md#environment-workflows). Admins grant it
/// roles like any user. The platform grants it none.
pub const WORKFLOW_USER_NAME: &str = "workflow";

/// The `platform."user"` id of the workflow user: the UUIDv5 of
/// [`WORKFLOW_USER_NAME`] under [`WAMN_NAMESPACE`].
pub fn workflow_user_id() -> uuid::Uuid {
    uuid::Uuid::new_v5(&WAMN_NAMESPACE, WORKFLOW_USER_NAME.as_bytes())
}

/// The built-in role with all served operations.
pub const ADMIN_ROLE: &str = "admin";
