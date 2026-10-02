/**
 * The administration screens of one application (docs/plan/platform-ui.md
 * §4.7), over the generated client `@wamn/control-client`, and the org and
 * project screens of Control (§4.4, §4.5), over `@wamn/control-org-client`.
 *
 * They are a subpath of `@wamn/ui`, so a page that places no administration
 * screen resolves no control client.
 */

export { OrgScreen, type OrgScreenProps } from "./org-screen";
export { ProjectScreen, type ProjectScreenProps } from "./project-screen";
export { operationInterface, RoleGrid, type RoleGridProps } from "./role-grid";
export { UserGrid, type UserGridProps } from "./user-grid";
