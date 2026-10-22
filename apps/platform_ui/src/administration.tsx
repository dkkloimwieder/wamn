import { permission, role, user, userRole } from "@wamn/control-client";
import { screen, type ShellSection } from "./shell";

export const administrationSections: readonly ShellSection[] = [
  {
    label: "Roles",
    screens: [
      {
        path: "administration/roles",
        label: "Roles",
        operation: role.ROLE_LIST_ROUTE.operation,
        component: screen(
          () => import("./application-roles"),
          (module) => module.ApplicationRoles,
        ),
        scroll: "content",
      },
    ],
    routes: [
      {
        path: "administration/roles/create",
        label: "Create role",
        operation: role.ROLE_CREATE_ROUTE.operation,
        component: screen(
          () => import("./application-roles"),
          (module) => module.CreateRole,
        ),
      },
      {
        path: "administration/roles/:role",
        operation: permission.PERMISSION_LIST_ROUTE.operation,
        component: screen(
          () => import("./application-roles"),
          (module) => module.RolePermissions,
        ),
        scroll: "content",
      },
      {
        path: "administration/roles/:role/permissions",
        label: "Permissions",
        operation: permission.PERMISSION_LIST_ROUTE.operation,
        component: screen(
          () => import("./application-roles"),
          (module) => module.RolePermissions,
        ),
        scroll: "content",
      },
      {
        path: "administration/roles/:role/users",
        label: "Users",
        operation: user.USER_LIST_ROUTE.operation,
        component: screen(
          () => import("./role-users"),
          (module) => module.RoleUsers,
        ),
        scroll: "content",
      },
      {
        path: "administration/roles/:role/users/grant",
        label: "Grant to user",
        operation: userRole.USER_ROLE_GRANT_ROUTE.operation,
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
        operation: user.USER_LIST_ROUTE.operation,
        component: screen(
          () => import("./application-users"),
          (module) => module.ApplicationUsers,
        ),
        scroll: "content",
      },
    ],
    routes: [
      {
        path: "administration/users/:principal",
        operation: user.USER_LIST_ROUTE.operation,
        component: screen(
          () => import("./application-users"),
          (module) => module.ApplicationUsers,
        ),
        scroll: "content",
      },
      {
        path: "administration/users/:principal/grant",
        label: "Grant role",
        operation: userRole.USER_ROLE_GRANT_ROUTE.operation,
        component: screen(
          () => import("./application-users"),
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
        operation: permission.PERMISSION_LIST_ROUTE.operation,
        component: screen(
          () => import("./application-roles"),
          (module) => module.PermissionsOverview,
        ),
        scroll: "content",
      },
    ],
  },
];
