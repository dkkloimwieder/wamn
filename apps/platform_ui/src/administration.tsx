import { permission, role, user, userRole } from "./app-client";
import { screen, type ShellSection } from "./shell";

export const administrationSections: readonly ShellSection[] = [
  {
    label: "Roles",
    screens: [
      {
        path: "administration/roles",
        label: "Roles",
        operation: role.list.route.operation,
        component: screen(
          () => import("./app-roles"),
          (module) => module.ApplicationRoles,
        ),
        scroll: "content",
      },
    ],
    routes: [
      {
        path: "administration/roles/create",
        label: "Create role",
        operation: role.create.route.operation,
        component: screen(
          () => import("./app-roles"),
          (module) => module.CreateRole,
        ),
      },
      {
        path: "administration/roles/:role",
        operation: permission.list.route.operation,
        component: screen(
          () => import("./app-roles"),
          (module) => module.RolePermissions,
        ),
        scroll: "content",
      },
      {
        path: "administration/roles/:role/permissions",
        label: "Permissions",
        operation: permission.list.route.operation,
        component: screen(
          () => import("./app-roles"),
          (module) => module.RolePermissions,
        ),
        scroll: "content",
      },
      {
        path: "administration/roles/:role/users",
        label: "Users",
        operation: user.list.route.operation,
        component: screen(
          () => import("./role-users"),
          (module) => module.RoleUsers,
        ),
        scroll: "content",
      },
      {
        path: "administration/roles/:role/users/grant",
        label: "Grant to user",
        operation: userRole.grant.route.operation,
        component: screen(
          () => import("./role-users"),
          (module) => module.GrantRoleToUser,
        ),
      },
    ],
  },
  {
    label: "Users",
    screens: [
      {
        path: "administration/users",
        label: "Users",
        operation: user.list.route.operation,
        component: screen(
          () => import("./app-users"),
          (module) => module.ApplicationUsers,
        ),
        scroll: "content",
      },
    ],
    routes: [
      {
        path: "administration/users/:principal",
        operation: user.list.route.operation,
        component: screen(
          () => import("./app-users"),
          (module) => module.ApplicationUsers,
        ),
        scroll: "content",
      },
      {
        path: "administration/users/:principal/grant",
        label: "Grant role",
        operation: userRole.grant.route.operation,
        component: screen(
          () => import("./app-users"),
          (module) => module.GrantApplicationRole,
        ),
      },
    ],
  },
  {
    label: "Permissions overview",
    screens: [
      {
        path: "administration/permissions",
        label: "Permissions overview",
        operation: permission.list.route.operation,
        component: screen(
          () => import("./app-roles"),
          (module) => module.PermissionsOverview,
        ),
        scroll: "content",
      },
    ],
  },
];
