import type { ReadResult } from "./errors";
import type { Draft, Report, Run } from "./types";

/**
 * The console's whole read surface: three resources, one read each, view-only.
 *
 * Resource-named after the authoring queries they will carry — `get-run`,
 * `get-report`, `read-draft`. Drafts are addressed by id and revision, matching
 * both the `#/draft/:id/:revision` route and the contract's `DraftRevisionRef`.
 */
export interface AuthoringReader {
  readonly runs: {
    get(runId: string): Promise<ReadResult<Run>>;
  };
  readonly reports: {
    get(reportId: string): Promise<ReadResult<Report>>;
  };
  readonly drafts: {
    get(draftId: string, revision: number): Promise<ReadResult<Draft>>;
  };
}
