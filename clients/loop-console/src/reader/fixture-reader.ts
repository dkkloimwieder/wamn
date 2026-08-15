import type { AuthoringReader } from "./contract";
import { err, ok, type ReadResult, type ReaderError } from "./errors";
import { draftFixtures, draftKey, reportFixtures, runFixtures } from "./fixtures";

/**
 * The fixture reader: every canned read, plus a reserved id per error state so
 * each ErrorPanel state is reachable from the running UI without code changes.
 *
 * The reserved ids work on all three resources — `#/run/network-down`,
 * `#/report/refused-authorization`, `#/draft/refused-contract-version/1`:
 *
 *   refused-authorization      → refusal, `authorization-denied`
 *   refused-contract-version   → refusal, `unsupported-contract-version`
 *   network-down               → network, transport never got a response
 *
 * Any other unknown id is the not-found refusal for its resource.
 */
const errorProbes: ReadonlyMap<string, ReaderError> = new Map([
  ["refused-authorization", { kind: "refusal", literal: "authorization-denied", detail: null }],
  [
    "refused-contract-version",
    {
      kind: "refusal",
      literal: "unsupported-contract-version",
      detail: "requested 0.2, supported 0.1",
    },
  ],
  [
    "network-down",
    {
      kind: "network",
      // The house wording for a request that never received an HTTP response.
      message: "authoring request failed before receiving an HTTP response",
    },
  ],
]);

function read<T>(id: string, found: T | undefined, missing: ReaderError): ReadResult<T> {
  const probe = errorProbes.get(id);
  if (probe !== undefined) {
    return err(probe);
  }
  return found === undefined ? err(missing) : ok(found);
}

export const fixtureReader: AuthoringReader = {
  runs: {
    async get(runId) {
      return read(runId, runFixtures.get(runId), {
        kind: "not-found",
        literal: "run-not-found",
        resource: "run",
        id: runId,
        revision: null,
      });
    },
  },
  reports: {
    async get(reportId) {
      return read(reportId, reportFixtures.get(reportId), {
        kind: "not-found",
        literal: "report-not-found",
        resource: "report",
        id: reportId,
        revision: null,
      });
    },
  },
  drafts: {
    async get(draftId, revision) {
      return read(draftId, draftFixtures.get(draftKey(draftId, revision)), {
        kind: "not-found",
        literal: "draft-revision-not-found",
        resource: "draft",
        id: draftId,
        revision,
      });
    },
  },
};
