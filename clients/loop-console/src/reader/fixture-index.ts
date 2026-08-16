import { draftFixtures, reportFixtures, runFixtures } from "./fixtures";
import type { Run } from "./types";
import type { Route } from "../routing/route";

/**
 * What the fixture reader happens to hold — a dev affordance, and deliberately
 * not part of `AuthoringReader`.
 *
 * §2.0's whole premise is that navigation is id-addressed because the platform
 * offers no list endpoints, so the contract cannot grow a `list()` that step 9's
 * HTTP reader would have nothing to implement. But the fixture reader is a map
 * this build ships, and against fixtures an author has no CLI to get an id from
 * and no way to discover that `01J9Y2QB…` is the truncation case. Enumerating a
 * map the console already contains is not a claim about the platform.
 *
 * This is why it lives beside the fixtures rather than behind the reader: it
 * disappears with them (`usingFixtures`), and nothing on the read path can come
 * to depend on a listing that will not exist in step 9.
 */

export type IndexEntry = {
  readonly route: Route;
  /** What an author types to get here — the id, or `id@revision` for a draft. */
  readonly address: string;
  /** Why this fixture exists, derived from the fixture itself so it cannot drift. */
  readonly note: string;
};

export type FixtureIndex = {
  readonly runs: readonly IndexEntry[];
  readonly reports: readonly IndexEntry[];
  readonly drafts: readonly IndexEntry[];
};

/** `VITE_READER=http` is the live reader; anything else, including unset, is fixtures. */
export function usingFixtures(
  mode: string | undefined = import.meta.env["VITE_READER"],
): boolean {
  return mode !== "http";
}

function runNote(run: Run): string {
  const parts: string[] = [run.status];
  // A terminalized run reads back as `failed` with an `effect-uncertain` kind,
  // so the kind is worth saying — but an effect-uncertain run's kind repeats its
  // own status, and `effect-uncertain · effect-uncertain` says nothing twice.
  const kind = run.failure?.kind;
  if (kind !== undefined && kind !== run.status) {
    parts.push(kind);
  }
  // What actually distinguishes the truncation fixture from any other run that
  // succeeded — otherwise three rows here read identically.
  if (run.factCount.truncated) {
    parts.push(`${run.facts.length} of ${run.factCount.total} facts`);
  }
  if (run.capture === "off") {
    parts.push("capture off");
  }
  return parts.join(" · ");
}

export function fixtureIndex(): FixtureIndex {
  const runs = [...runFixtures.values()].map<IndexEntry>((run) => ({
    route: { kind: "run", id: run.runId },
    address: run.runId,
    note: runNote(run),
  }));

  const reports = [...reportFixtures.values()].map<IndexEntry>((report) => ({
    route: { kind: "report", id: report.reportId },
    address: report.reportId,
    // A pending report has no verdict yet, so it is not `0 / n` — that would
    // read as a report that finalized having passed nothing.
    note:
      report.state === "pending"
        ? "pending"
        : `${report.passed} / ${report.total} passed`,
  }));

  const drafts = [...draftFixtures.values()].map<IndexEntry>((draft) => ({
    route: { kind: "draft", id: draft.draftId, revision: String(draft.revision) },
    address: `${draft.draftId}@${draft.revision}`,
    // Whether it parses is the thing that distinguishes §2.4's cases, and a
    // draft that does not parse is a normal object here rather than an error.
    note: draft.parse.ok ? `${draft.flowId} · parses` : `${draft.flowId} · does not parse`,
  }));

  return { runs, reports, drafts };
}
