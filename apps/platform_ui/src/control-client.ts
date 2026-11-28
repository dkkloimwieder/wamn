import document from "../../target/wamn/platform_ui/control.openapi.json";
import * as sdk from "./generated/wamn_control_org/index.js";
import { hostOperations, type OperationResult } from "./host-client";

const bind = hostOperations(document);

const paths = document.paths;
export const controlFields = {
  deploy:
    paths["/wamn_control/app/deploy"].post.requestBody.content["application/json"].schema.items.properties.value
      .properties,
  stop: paths["/wamn_control/app/stop"].post.requestBody.content["application/json"].schema.items.properties
    .value.properties,
  rollback:
    paths["/wamn_control/app/rollback"].post.requestBody.content["application/json"].schema.items.properties
      .value.properties,
  diff: paths["/wamn_control/schema/diff"].post.requestBody.content["application/json"].schema.items.properties.value
    .properties,
  apply:
    paths["/wamn_control/schema/apply"].post.requestBody.content["application/json"].schema.items.properties.value
      .properties,
  createProject:
    paths["/wamn_control/project/create"].post.requestBody.content["application/json"].schema.items.properties.value
      .properties,
  inviteUser:
    paths["/wamn_control/user/invite"].post.requestBody.content["application/json"].schema.items.properties.value
      .properties,
  grantMember:
    paths["/wamn_control/member/grant"].post.requestBody.content["application/json"].schema.items.properties.value
      .properties,
  current:
    paths["/wamn_control/control/mine"].get.responses["200"].content["application/json"].schema.items.oneOf[0]!
      .properties.value!.properties,
  env:
    paths["/wamn_control/env/list"].get.responses["200"].content["application/json"].schema.items.oneOf[0]!
      .properties.value!.properties.envs.items.properties,
  member:
    paths["/wamn_control/member/list"].get.responses["200"].content["application/json"].schema.items.oneOf[0]!
      .properties.value!.properties.members.items.properties,
  operation:
    paths["/wamn_control/env/operation_status"].get.responses["200"].content["application/json"].schema.items
      .oneOf[0]!.properties.value!.properties,
  user: paths["/wamn_control/user/list"].get.responses["200"].content["application/json"].schema.items.oneOf[0]!
    .properties.value!.properties.users.items.properties,
  status:
    paths["/wamn_control/env/status"].get.responses["200"].content["application/json"].schema.items.oneOf[0]!
      .properties.value!.properties,
};

export const appRoutes = {
  mine: bind("app.mine", sdk.wamnControlAppMine_0100),
  setDefault: bind("app.default", sdk.wamnControlAppDefault_0100),
  deploy: bind("app.deploy", sdk.wamnControlAppDeploy_0100),
  stop: bind("app.stop", sdk.wamnControlAppStop_0100),
  rollback: bind("app.rollback", sdk.wamnControlAppRollback_0100),
};
export type AppMineResult = OperationResult<typeof appRoutes.mine>;

export const control = {
  mine: bind("control.mine", sdk.wamnControlControlMine_0100),
};
export type ControlMineResult = OperationResult<typeof control.mine>;

export const envRoutes = {
  activate: bind("env.activate", sdk.wamnControlEnvActivate_0100),
  deactivate: bind("env.deactivate", sdk.wamnControlEnvDeactivate_0100),
  list: bind("env.list", sdk.wamnControlEnvList_0100),
  status: bind("env.status", sdk.wamnControlEnvStatus_0100),
};
export type EnvListResultEnvs = OperationResult<typeof envRoutes.list>["envs"][number];
export type EnvStatusResult = OperationResult<typeof envRoutes.status>;

export const member = {
  grant: bind("member.grant", sdk.wamnControlMemberGrant_0100),
  list: bind("member.list", sdk.wamnControlMemberList_0100),
  revoke: bind("member.revoke", sdk.wamnControlMemberRevoke_0100),
};
export type MemberListResultMembers = OperationResult<typeof member.list>["members"][number];

export const operation = {
  status: bind("operation.status", sdk.wamnControlOperationStatus_0100),
};
export type OperationStatusResult = OperationResult<typeof operation.status>;

export const orgAdmin = {
  grant: bind("org_admin.grant", sdk.wamnControlOrgAdminGrant_0100),
  revoke: bind("org_admin.revoke", sdk.wamnControlOrgAdminRevoke_0100),
};

export const project = {
  activate: bind("project.activate", sdk.wamnControlProjectActivate_0100),
  create: bind("project.create", sdk.wamnControlProjectCreate_0100),
  deactivate: bind("project.deactivate", sdk.wamnControlProjectDeactivate_0100),
  list: bind("project.list", sdk.wamnControlProjectList_0100),
};
export type ProjectListResultProjects = OperationResult<typeof project.list>["projects"][number];

export const projectAdmin = {
  grant: bind("project_admin.grant", sdk.wamnControlProjectAdminGrant_0100),
  revoke: bind("project_admin.revoke", sdk.wamnControlProjectAdminRevoke_0100),
};

export const schema = {
  apply: bind("schema.apply", sdk.wamnControlSchemaApply_0100),
  diff: bind("schema.diff", sdk.wamnControlSchemaDiff_0100),
  reset: bind("schema.reset", sdk.wamnControlSchemaReset_0100),
};

export const user = {
  activate: bind("user.activate", sdk.wamnControlUserActivate_0100),
  deactivate: bind("user.deactivate", sdk.wamnControlUserDeactivate_0100),
  invite: bind("user.invite", sdk.wamnControlUserInvite_0100),
  list: bind("user.list", sdk.wamnControlUserList_0100),
};
export type UserListResult = OperationResult<typeof user.list>;
export type UserListResultUsers = OperationResult<typeof user.list>["users"][number];
