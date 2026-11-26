import document from "../../target/wamn/platform_ui/app.openapi.json";
import * as sdk from "./generated/wamn_control/index.js";
import { hostOperations, type OperationResult } from "./host-client";

const bind = hostOperations(document);

const paths = document.paths;
export const appFields = {
  createRole:
    paths["/wamn_control/role/create"].post.requestBody.content["application/json"].schema.items.properties.value
      .properties,
  grantRole:
    paths["/wamn_control/user_role/grant"].post.requestBody.content["application/json"].schema.items.properties.value
      .properties,
  permissions:
    paths["/wamn_control/permission/list"].get.responses["200"].content["application/json"].schema.items.oneOf[0]!
      .properties.value!.properties,
  user: paths["/wamn_control/user/list"].get.responses["200"].content["application/json"].schema.items.oneOf[0]!
    .properties.value!.properties.users.items.properties,
};

export const permission = {
  list: bind("permission.list", sdk.wamnControlPermissionList_0100),
  mine: bind("permission.mine", sdk.wamnControlPermissionMine_0100),
  grant: bind("permission.grant", sdk.wamnControlPermissionGrant_0100),
  revoke: bind("permission.revoke", sdk.wamnControlPermissionRevoke_0100),
};
export type PermissionListResult = OperationResult<typeof permission.list>;

export const role = {
  list: bind("role.list", sdk.wamnControlRoleList_0100),
  create: bind("role.create", sdk.wamnControlRoleCreate_0100),
  delete_: bind("role.delete", sdk.wamnControlRoleDelete_0100),
};

export const user = {
  list: bind("user.list", sdk.wamnControlUserList_0100),
};
export type UserListResult = OperationResult<typeof user.list>;

export const userRole = {
  grant: bind("user_role.grant", sdk.wamnControlUserRoleGrant_0100),
  revoke: bind("user_role.revoke", sdk.wamnControlUserRoleRevoke_0100),
};
