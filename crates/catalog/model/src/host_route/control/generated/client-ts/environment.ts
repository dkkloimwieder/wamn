// @generated from the client-contract IR; do not edit.
//
// `environment` operations of package `wamn_control`.

import type { FieldMap, JsonValue, OperationRoute, Outcome, Timestamptz, Transport } from "@wamn/web-runtime";
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

/** Input for `wamn-control:environment/create@0.3.0`. */
export interface EnvironmentCreateRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: EnvironmentCreateRequestValue;
}

export interface EnvironmentCreateRequestValue {
  /** `array` */
  connections: EnvironmentCreateRequestValueConnections[];
  /** `text` */
  env: string;
  /** `array` */
  packages: EnvironmentCreateRequestValuePackages[];
  /** `text` */
  project: string;
  /** `text` */
  routeHost: string;
  /** `text` */
  tenant: string;
}

export interface EnvironmentCreateRequestValueConnections {
  /** `text` */
  alias: string;
  /** `json` */
  definition: JsonValue;
  /** `text` */
  instanceId: string;
  /** `text` */
  requirementType: "blobstore";
}

export interface EnvironmentCreateRequestValuePackages {
  /** `text` */
  packageId: string;
  /** `text` */
  version: string;
}

/** What `wamn-control:environment/create@0.3.0` calls its input members. */
export const ENVIRONMENT_CREATE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "connections": {
        member: "connections",
        fields: {
          "alias": "alias",
          "definition": "definition",
          "instance_id": "instanceId",
          "requirement_type": "requirementType",
        },
      },
      "env": "env",
      "packages": {
        member: "packages",
        fields: {
          "package_id": "packageId",
          "version": "version",
        },
      },
      "project": "project",
      "route_host": "routeHost",
      "tenant": "tenant",
    },
  },
};

/** Result of `wamn-control:environment/create@0.3.0`. */
export interface EnvironmentCreateResult {
  /** `text` */
  readonly sagaId: string;
}

/** What `wamn-control:environment/create@0.3.0` calls its result members. */
export const ENVIRONMENT_CREATE_RESULT_FIELDS: FieldMap = {
  "saga_id": "sagaId",
};

/**
 * Where the release publishes `wamn-control:environment/create@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const ENVIRONMENT_CREATE_ROUTE: OperationRoute = {
  operation: "wamn-control:environment/create@0.3.0",
  method: "POST",
  template: "/wamn_control/environment/create",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "environment_exists", required: ["field"], sources: ["transaction_invariant"], text: "This environment exists, or a saga that creates it is open." },
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

/** Invoke `wamn-control:environment/create@0.3.0` through a transport the application supplies. */
export async function create(
  transport: Transport,
  items: readonly EnvironmentCreateRequest[],
): Promise<Outcome<EnvironmentCreateResult>> {
  return reviveOutcome<EnvironmentCreateResult>(
    await transport.invoke({
      ...ENVIRONMENT_CREATE_ROUTE,
      items: items.map((item) => toWire(item, ENVIRONMENT_CREATE_REQUEST_FIELDS)),
    }),
    ENVIRONMENT_CREATE_RESULT_FIELDS,
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
  /** `array` */
  readonly sagas: readonly EnvironmentListResultSagas[];
}

export interface EnvironmentListResultSagas {
  /** `text` */
  readonly env: string;
  /** `text` */
  readonly lastError: string | null;
  /** `text` */
  readonly sagaId: string;
  /** `text` */
  readonly status: string;
  /** `array` */
  readonly steps: readonly EnvironmentListResultSagasSteps[];
}

export interface EnvironmentListResultSagasSteps {
  /** `json` */
  readonly detail: JsonValue | null;
  /** `text` */
  readonly error: string | null;
  /** `timestamptz` */
  readonly finishedAt: Timestamptz | null;
  /** `text` */
  readonly name: string;
  /** `timestamptz` */
  readonly startedAt: Timestamptz | null;
  /** `text` */
  readonly status: string;
  /** `int32` */
  readonly step: number;
}

/** What `wamn-control:environment/list@0.3.0` calls its result members. */
export const ENVIRONMENT_LIST_RESULT_FIELDS: FieldMap = {
  "environments": "environments",
  "sagas": {
    member: "sagas",
    fields: {
      "env": "env",
      "last_error": "lastError",
      "saga_id": "sagaId",
      "status": "status",
      "steps": {
        member: "steps",
        fields: {
          "detail": "detail",
          "error": "error",
          "finished_at": "finishedAt",
          "name": "name",
          "started_at": "startedAt",
          "status": "status",
          "step": "step",
        },
      },
    },
  },
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
