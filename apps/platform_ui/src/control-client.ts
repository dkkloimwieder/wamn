import document from "../../target/wamn/platform_ui/control.openapi.json";
import * as sdk from "./generated/wamn_control_org/index.js";
import { hostOperations, type OperationResult } from "./host-client";

const bind = hostOperations(document);

export const application = {
  deploy: bind("application.deploy", sdk.wamnControlApplicationDeploy_090),
  stop: bind("application.stop", sdk.wamnControlApplicationStop_090),
  rollback: bind("application.rollback", sdk.wamnControlApplicationRollback_090),
};

export const control = {
  mine: bind("control.mine", sdk.wamnControlControlMine_090),
};
export type ControlMineResult = OperationResult<typeof control.mine>;

export const environment = {
  activate: bind("environment.activate", sdk.wamnControlEnvironmentActivate_090),
  inactivate: bind("environment.inactivate", sdk.wamnControlEnvironmentInactivate_090),
  list: bind("environment.list", sdk.wamnControlEnvironmentList_090),
  status: bind("environment.status", sdk.wamnControlEnvironmentStatus_090),
};
export type EnvironmentListResultEnvironments = OperationResult<typeof environment.list>["environments"][number];
export type EnvironmentStatusResult = OperationResult<typeof environment.status>;

export const member = {
  grant: bind("member.grant", sdk.wamnControlMemberGrant_090),
  list: bind("member.list", sdk.wamnControlMemberList_090),
  revoke: bind("member.revoke", sdk.wamnControlMemberRevoke_090),
};
export type MemberListResultMembers = OperationResult<typeof member.list>["members"][number];

export const operation = {
  status: bind("operation.status", sdk.wamnControlOperationStatus_090),
};
export type OperationStatusResult = OperationResult<typeof operation.status>;

export const orgAdmin = {
  grant: bind("org_admin.grant", sdk.wamnControlOrgAdminGrant_090),
  revoke: bind("org_admin.revoke", sdk.wamnControlOrgAdminRevoke_090),
};

export const project = {
  activate: bind("project.activate", sdk.wamnControlProjectActivate_090),
  create: bind("project.create", sdk.wamnControlProjectCreate_090),
  inactivate: bind("project.inactivate", sdk.wamnControlProjectInactivate_090),
  list: bind("project.list", sdk.wamnControlProjectList_090),
};
export type ProjectListResultProjects = OperationResult<typeof project.list>["projects"][number];

export const projectAdmin = {
  grant: bind("project_admin.grant", sdk.wamnControlProjectAdminGrant_090),
  revoke: bind("project_admin.revoke", sdk.wamnControlProjectAdminRevoke_090),
};

export const schema = {
  apply: bind("schema.apply", sdk.wamnControlSchemaApply_090),
  diff: bind("schema.diff", sdk.wamnControlSchemaDiff_090),
  reset: bind("schema.reset", sdk.wamnControlSchemaReset_090),
};

export const user = {
  activate: bind("user.activate", sdk.wamnControlUserActivate_090),
  deactivate: bind("user.deactivate", sdk.wamnControlUserDeactivate_090),
  invite: bind("user.invite", sdk.wamnControlUserInvite_090),
  list: bind("user.list", sdk.wamnControlUserList_090),
};
export type UserListResult = OperationResult<typeof user.list>;
export type UserListResultUsers = OperationResult<typeof user.list>["users"][number];
