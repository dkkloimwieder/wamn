/**
 * The administration screens of one application (docs/plan/platform-ui.md
 * §4.7), over the generated client `@wamn/control-client`.
 *
 * They are a subpath of `@wamn/ui`, so a page that places no administration
 * screen resolves no control client.
 */

export { operationInterface, RoleGrid, type RoleGridProps } from "./role-grid";
export { UserGrid, type UserGridProps } from "./user-grid";
