// @generated from the client-contract IR; do not edit.
//
// `project` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:project/activate@0.2.0`. */
export interface ProjectActivateRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: ProjectActivateRequestValue;
}

export interface ProjectActivateRequestValue {
  /** `text` */
  project: string;
}

/** What `wamn-control:project/activate@0.2.0` calls its input members. */
export const PROJECT_ACTIVATE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "project": "project",
    },
  },
};

/** Result of `wamn-control:project/activate@0.2.0`. */
export interface ProjectActivateResult {
  /** `text` */
  readonly project: string;
  /** `text` */
  readonly status: string;
}

/** What `wamn-control:project/activate@0.2.0` calls its result members. */
export const PROJECT_ACTIVATE_RESULT_FIELDS: FieldMap = {
  "project": "project",
  "status": "status",
};

/**
 * Where the release publishes `wamn-control:project/activate@0.2.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PROJECT_ACTIVATE_ROUTE: OperationRoute = {
  operation: "wamn-control:project/activate@0.2.0",
  method: "POST",
  template: "/wamn_control/project/activate",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "application_write_incomplete", required: ["environment"], sources: ["transaction_invariant"], text: "The application rows stopped at this environment. The completed environments are done." },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "project_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This project is not a project of the org." },
    ],
    replay: null,
    direct: true,
    type: "command",
    transaction: "explicit_per_input",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:project/activate@0.2.0` through a transport the application supplies. */
export async function activate(
  transport: Transport,
  items: readonly ProjectActivateRequest[],
): Promise<Outcome<ProjectActivateResult>> {
  return reviveOutcome<ProjectActivateResult>(
    await transport.invoke({
      ...PROJECT_ACTIVATE_ROUTE,
      items: items.map((item) => toWire(item, PROJECT_ACTIVATE_REQUEST_FIELDS)),
    }),
    PROJECT_ACTIVATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:project/inactivate@0.2.0`. */
export interface ProjectInactivateRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: ProjectInactivateRequestValue;
}

export interface ProjectInactivateRequestValue {
  /** `text` */
  project: string;
}

/** What `wamn-control:project/inactivate@0.2.0` calls its input members. */
export const PROJECT_INACTIVATE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "project": "project",
    },
  },
};

/** Result of `wamn-control:project/inactivate@0.2.0`. */
export interface ProjectInactivateResult {
  /** `text` */
  readonly project: string;
  /** `text` */
  readonly status: string;
}

/** What `wamn-control:project/inactivate@0.2.0` calls its result members. */
export const PROJECT_INACTIVATE_RESULT_FIELDS: FieldMap = {
  "project": "project",
  "status": "status",
};

/**
 * Where the release publishes `wamn-control:project/inactivate@0.2.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PROJECT_INACTIVATE_ROUTE: OperationRoute = {
  operation: "wamn-control:project/inactivate@0.2.0",
  method: "POST",
  template: "/wamn_control/project/inactivate",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "application_write_incomplete", required: ["environment"], sources: ["transaction_invariant"], text: "The application rows stopped at this environment. The completed environments are done." },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "project_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This project is not a project of the org." },
    ],
    replay: null,
    direct: true,
    type: "command",
    transaction: "explicit_per_input",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:project/inactivate@0.2.0` through a transport the application supplies. */
export async function inactivate(
  transport: Transport,
  items: readonly ProjectInactivateRequest[],
): Promise<Outcome<ProjectInactivateResult>> {
  return reviveOutcome<ProjectInactivateResult>(
    await transport.invoke({
      ...PROJECT_INACTIVATE_ROUTE,
      items: items.map((item) => toWire(item, PROJECT_INACTIVATE_REQUEST_FIELDS)),
    }),
    PROJECT_INACTIVATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:project/list@0.2.0`. */
export interface ProjectListRequest {
}

/** What `wamn-control:project/list@0.2.0` calls its input members. */
export const PROJECT_LIST_REQUEST_FIELDS: FieldMap = {};

/** Result of `wamn-control:project/list@0.2.0`. */
export interface ProjectListResult {
  /** `array` */
  readonly projects: readonly string[];
}

/** What `wamn-control:project/list@0.2.0` calls its result members. */
export const PROJECT_LIST_RESULT_FIELDS: FieldMap = {
  "projects": "projects",
};

/**
 * Where the release publishes `wamn-control:project/list@0.2.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PROJECT_LIST_ROUTE: OperationRoute = {
  operation: "wamn-control:project/list@0.2.0",
  method: "GET",
  template: "/wamn_control/project/list",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
    ],
    replay: null,
    direct: true,
    type: "get",
    transaction: "implicit",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:project/list@0.2.0` through a transport the application supplies. */
export async function list(
  transport: Transport,
  items: readonly ProjectListRequest[],
): Promise<Outcome<ProjectListResult>> {
  return reviveOutcome<ProjectListResult>(
    await transport.invoke({
      ...PROJECT_LIST_ROUTE,
      items: items.map((item) => toWire(item, PROJECT_LIST_REQUEST_FIELDS)),
    }),
    PROJECT_LIST_RESULT_FIELDS,
  );
}
