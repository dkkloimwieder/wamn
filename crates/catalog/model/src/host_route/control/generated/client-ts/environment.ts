// @generated from the client-contract IR; do not edit.
//
// `environment` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:environment/activate@0.3.0`. */
export interface EnvironmentActivateRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: EnvironmentActivateRequestValue;
}

export interface EnvironmentActivateRequestValue {
  /** `text` */
  env: string;
  /** `text` */
  project: string;
}

/** What `wamn-control:environment/activate@0.3.0` calls its input members. */
export const ENVIRONMENT_ACTIVATE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "env": "env",
      "project": "project",
    },
  },
};

/** Result of `wamn-control:environment/activate@0.3.0`. */
export interface EnvironmentActivateResult {
  /** `text` */
  readonly env: string;
  /** `text` */
  readonly project: string;
  /** `text` */
  readonly status: string;
}

/** What `wamn-control:environment/activate@0.3.0` calls its result members. */
export const ENVIRONMENT_ACTIVATE_RESULT_FIELDS: FieldMap = {
  "env": "env",
  "project": "project",
  "status": "status",
};

/**
 * Where the release publishes `wamn-control:environment/activate@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const ENVIRONMENT_ACTIVATE_ROUTE: OperationRoute = {
  operation: "wamn-control:environment/activate@0.3.0",
  method: "POST",
  template: "/wamn_control/environment/activate",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "application_write_incomplete", required: ["environment"], sources: ["transaction_invariant"], text: "The application rows stopped at this environment. The completed environments are done." },
      { literal: "environment_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This environment is not an environment of the org." },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
    ],
    replay: null,
    direct: true,
    type: "command",
    transaction: "explicit_per_input",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:environment/activate@0.3.0` through a transport the application supplies. */
export async function activate(
  transport: Transport,
  items: readonly EnvironmentActivateRequest[],
): Promise<Outcome<EnvironmentActivateResult>> {
  return reviveOutcome<EnvironmentActivateResult>(
    await transport.invoke({
      ...ENVIRONMENT_ACTIVATE_ROUTE,
      items: items.map((item) => toWire(item, ENVIRONMENT_ACTIVATE_REQUEST_FIELDS)),
    }),
    ENVIRONMENT_ACTIVATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:environment/inactivate@0.3.0`. */
export interface EnvironmentInactivateRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: EnvironmentInactivateRequestValue;
}

export interface EnvironmentInactivateRequestValue {
  /** `text` */
  env: string;
  /** `text` */
  project: string;
}

/** What `wamn-control:environment/inactivate@0.3.0` calls its input members. */
export const ENVIRONMENT_INACTIVATE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "env": "env",
      "project": "project",
    },
  },
};

/** Result of `wamn-control:environment/inactivate@0.3.0`. */
export interface EnvironmentInactivateResult {
  /** `text` */
  readonly env: string;
  /** `text` */
  readonly project: string;
  /** `text` */
  readonly status: string;
}

/** What `wamn-control:environment/inactivate@0.3.0` calls its result members. */
export const ENVIRONMENT_INACTIVATE_RESULT_FIELDS: FieldMap = {
  "env": "env",
  "project": "project",
  "status": "status",
};

/**
 * Where the release publishes `wamn-control:environment/inactivate@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const ENVIRONMENT_INACTIVATE_ROUTE: OperationRoute = {
  operation: "wamn-control:environment/inactivate@0.3.0",
  method: "POST",
  template: "/wamn_control/environment/inactivate",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "application_write_incomplete", required: ["environment"], sources: ["transaction_invariant"], text: "The application rows stopped at this environment. The completed environments are done." },
      { literal: "environment_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This environment is not an environment of the org." },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
    ],
    replay: null,
    direct: true,
    type: "command",
    transaction: "explicit_per_input",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:environment/inactivate@0.3.0` through a transport the application supplies. */
export async function inactivate(
  transport: Transport,
  items: readonly EnvironmentInactivateRequest[],
): Promise<Outcome<EnvironmentInactivateResult>> {
  return reviveOutcome<EnvironmentInactivateResult>(
    await transport.invoke({
      ...ENVIRONMENT_INACTIVATE_ROUTE,
      items: items.map((item) => toWire(item, ENVIRONMENT_INACTIVATE_REQUEST_FIELDS)),
    }),
    ENVIRONMENT_INACTIVATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:environment/list@0.3.0`. */
export interface EnvironmentListRequest {
  /** `text` */
  project: string;
}

/** What `wamn-control:environment/list@0.3.0` calls its input members. */
export const ENVIRONMENT_LIST_REQUEST_FIELDS: FieldMap = {
  "project": "project",
};

/** Result of `wamn-control:environment/list@0.3.0`. */
export interface EnvironmentListResult {
  /** `array` */
  readonly environments: readonly string[];
}

/** What `wamn-control:environment/list@0.3.0` calls its result members. */
export const ENVIRONMENT_LIST_RESULT_FIELDS: FieldMap = {
  "environments": "environments",
};

/**
 * Where the release publishes `wamn-control:environment/list@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const ENVIRONMENT_LIST_ROUTE: OperationRoute = {
  operation: "wamn-control:environment/list@0.3.0",
  method: "GET",
  template: "/wamn_control/environment/list",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
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

/** Invoke `wamn-control:environment/list@0.3.0` through a transport the application supplies. */
export async function list(
  transport: Transport,
  items: readonly EnvironmentListRequest[],
): Promise<Outcome<EnvironmentListResult>> {
  return reviveOutcome<EnvironmentListResult>(
    await transport.invoke({
      ...ENVIRONMENT_LIST_ROUTE,
      items: items.map((item) => toWire(item, ENVIRONMENT_LIST_REQUEST_FIELDS)),
    }),
    ENVIRONMENT_LIST_RESULT_FIELDS,
  );
}
