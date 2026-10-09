// @generated from the client-contract IR; do not edit.
//
// `package` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Timestamptz, Transport } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:package/list@0.5.0`. */
export interface PackageListRequest {
}

/** What `wamn-control:package/list@0.5.0` calls its input members. */
export const PACKAGE_LIST_REQUEST_FIELDS: FieldMap = {};

/** Result of `wamn-control:package/list@0.5.0`. */
export interface PackageListResult {
  /** `array` */
  readonly packages: readonly PackageListResultPackages[];
}

export interface PackageListResultPackages {
  /** `timestamptz` */
  readonly attestedAt: Timestamptz;
  /** `text` */
  readonly packageId: string;
  /** `text` */
  readonly version: string;
}

/** What `wamn-control:package/list@0.5.0` calls its result members. */
export const PACKAGE_LIST_RESULT_FIELDS: FieldMap = {
  "packages": {
    member: "packages",
    fields: {
      "attested_at": "attestedAt",
      "package_id": "packageId",
      "version": "version",
    },
  },
};

/**
 * Where the release publishes `wamn-control:package/list@0.5.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PACKAGE_LIST_ROUTE: OperationRoute = {
  operation: "wamn-control:package/list@0.5.0",
  method: "GET",
  template: "/wamn_control/package/list",
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

/** Invoke `wamn-control:package/list@0.5.0` through a transport the application supplies. */
export async function list(
  transport: Transport,
  items: readonly PackageListRequest[],
): Promise<Outcome<PackageListResult>> {
  return reviveOutcome<PackageListResult>(
    await transport.invoke({
      ...PACKAGE_LIST_ROUTE,
      items: items.map((item) => toWire(item, PACKAGE_LIST_REQUEST_FIELDS)),
    }),
    PACKAGE_LIST_RESULT_FIELDS,
  );
}
