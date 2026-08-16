import type { JSX } from "@solidjs/web";
import {
  Errored,
  Loading,
  Match,
  Show,
  Switch,
  createMemo,
  createSignal,
  isPending,
  onCleanup,
} from "solid-js";

import { setReadStatus, type ReadStatus } from "../app/read-status";
import { contributeScreen } from "../app/screen-actions";
import type { ReadResult, ReaderError } from "../reader/errors";
import { selectReader } from "../reader/reader";
import type { Report } from "../reader/types";
import { recordVisit, reportVerdictState, visitFromReport } from "../store/visited";
import { CopyId } from "../ui/copy-id";
import { ErrorPanel } from "../ui/error-panel";
import { RefreshControl } from "../ui/refresh-control";
import { SectionLabel } from "../ui/section-label";
import { EmptyState, LoadingBlock } from "../ui/states";
import { verdictTone, type Tone } from "../ui/status";
import { VerdictBar } from "../ui/verdict-bar";
import { ReportCases } from "./report-cases";
import "./report-screen.css";

/**
 * §2.3's report screen: one read of one report, then the answer before the eye
 * moves — *did the set pass → which case failed → on what expectation → take me
 * to its run*.
 *
 * The screen owns the read and nothing else, exactly as the run screen does:
 * the case list takes the report's cases and renders them, and this file decides
 * what each of the read's three outcomes looks like.
 */
export function ReportScreen(props: { id: string }): JSX.Element {
  const reader = selectReader();
  /** §2.6's refresh: re-issuing the screen's one read is bumping this. */
  const [issued, setIssued] = createSignal(0);
  const [refreshedAt, setRefreshedAt] = createSignal<Date | null>(null);
  /*
   * Which read the writes below belong to, as plain values rather than signals:
   * they are consulted after an await, where a signal read tracks nothing and a
   * signal write would land in the memo's owned scope.
   */
  let latest = 0;
  let disposed = false;
  onCleanup(() => {
    disposed = true;
  });

  const read = createMemo(async () => {
    issued();
    const issue = (latest += 1);
    const result = await reader.reports.get(props.id);
    /*
     * §1.4's status dot describes the *last* read, so an older answer may not
     * overwrite a newer one, and a screen the author has already left may not
     * move global state at all.
     */
    if (disposed || issue !== latest) {
      return result;
    }
    setReadStatus(readStatusOf(result));
    setRefreshedAt(new Date());
    // §6's "4–5 write it": the report is in the reader's history from the moment
    // the platform answers. Only a report that came back — a refusal names no
    // entity to remember.
    if (result.ok) {
      recordVisit(visitFromReport(result.value, Date.now()));
    }
    return result;
  });

  const refresh = (): void => {
    setIssued((count) => count + 1);
  };

  return (
    <section class="report-screen">
      {/* A fault in a part must not blank the shell around it (§1.4). */}
      <Errored
        fallback={(fault) => (
          <p class="report-fault frame">the report screen failed to render: {String(fault())}</p>
        )}
      >
        <Loading fallback={<LoadingBlock region="the report" />}>
          <ReportBody
            read={read}
            onRefresh={refresh}
            refresh={
              <RefreshControl
                refreshedAt={refreshedAt()}
                busy={isPending(read)}
                onRefresh={refresh}
              />
            }
          />
        </Loading>
      </Errored>
    </section>
  );
}

/**
 * §1.4's dot, from one read. A `not-found` is a 2xx answer carrying a literal
 * (`reader/errors.ts`), so mistyping a report id must not tell the author the
 * console cannot reach the target. Every other refusal is a read the author did
 * not get an answer to.
 */
function readStatusOf(result: ReadResult<Report>): ReadStatus {
  return result.ok || result.error.kind === "not-found" ? "ok" : "fail";
}

/** The reader never throws, so both outcomes of a settled read are values here. */
function readReport(result: ReadResult<Report>): Report | null {
  return result.ok ? result.value : null;
}

function readRefusal(result: ReadResult<Report>): ReaderError | null {
  return result.ok ? null : result.error;
}

function ReportBody(props: {
  read: () => ReadResult<Report>;
  refresh: JSX.Element;
  onRefresh: () => void;
}): JSX.Element {
  return (
    <Switch>
      <Match when={readReport(props.read())}>
        {(report) => (
          <ReportFound report={report()} refresh={props.refresh} onRefresh={props.onRefresh} />
        )}
      </Match>
      <Match when={readRefusal(props.read())}>
        {(error) => (
          <ReportRefused error={error()} refresh={props.refresh} onRefresh={props.onRefresh} />
        )}
      </Match>
    </Switch>
  );
}

/**
 * The refused read still offers the one control that could change the answer, to
 * the palette as well as to the mouse — §6 step 7 leaves the palette as the
 * keyboard's only route to it.
 */
function ReportRefused(props: {
  error: ReaderError;
  refresh: JSX.Element;
  onRefresh: () => void;
}): JSX.Element {
  contributeScreen(() => ({
    anchors: [],
    actions: [{ id: "refresh", label: "refresh this screen", run: props.onRefresh }],
  }));

  return (
    <div class="report-section">
      <SectionLabel label="report" trailing={props.refresh} />
      <ErrorPanel error={props.error} />
    </div>
  );
}

/** One variant of the read, or null: what `<Match>` needs so its callback binds a narrowed report. */
function finalized(report: Report): Extract<Report, { state: "finalized" }> | null {
  return report.state === "finalized" ? report : null;
}

function pending(report: Report): Extract<Report, { state: "pending" }> | null {
  return report.state === "pending" ? report : null;
}

/** A pending report whose read carried no reason — the state §2.3's line cannot print. */
function unstatedPendingReason(report: Report): boolean {
  return report.state === "pending" && report.pendingReason === null;
}

/**
 * §2.3's verdict line: `11 / 12 PASSED`.
 *
 * Pending is the honest departure from the spec. §2.3 asks for "the contracted
 * reason as the verdict line", and there is no contracted reason: the pending
 * projection is three fields (`report-id`, `validated-draft`, `test-set`) and
 * none of them carries one, which is why `pendingReason` is nullable and null in
 * practice. The verdict line is the one place on the screen an author reads
 * without looking further, so it may not be filled in with a plausible sentence —
 * the state word stands alone and the absence is stated under the bar. The null
 * branch is not dead code: a projection that grows a reason later prints it here.
 *
 * A finalized report with nothing in it takes neither arithmetic. `0 / 0 PASSED`
 * is the zero standing in for an absence — a green all-pass over a set that
 * never ran, directly above a list saying the report finalized with no cases —
 * and both counts are reader-synthesized from the untyped `summary`
 * (`types.ts` STEP-9 RISK), so an empty set is a state a read can hand us. The
 * bar says what it has instead of doing sums on nothing.
 */
export function reportVerdictLine(report: Report): string {
  if (report.state === "pending") {
    return report.pendingReason === null ? "PENDING" : `PENDING · ${report.pendingReason}`;
  }
  if (report.total === 0) {
    return "NO CASES RECORDED";
  }
  return `${report.passed} / ${report.total} PASSED`;
}

/**
 * §2.3: all-pass renders `ok`, any-fail `fail`, pending `warn`.
 *
 * The finalized tone is taken from the visited store's own verdict, so the tree
 * glyph and the bar can never disagree about a report. Pending is not: the tree
 * has no verdict for a report that has not finalized (`none`, a neutral dot) and
 * the bar says the author is waiting on one — both true, differently.
 *
 * The empty finalized set is the one place the bar refuses the store's answer.
 * `reportVerdictState` reads `passed === total` as a pass, which is `ok` when
 * both are zero; the bar has no verdict to give there and takes the neutral one
 * its line already states in words. The tree glyph would still draw `✓` for such
 * a report — the store's arithmetic is not this screen's to change, and the
 * disagreement is worth naming rather than a green verdict bar.
 */
export function reportVerdictTone(report: Report): Tone {
  if (report.state === "pending") {
    return "warn";
  }
  return report.total === 0 ? "neutral" : verdictTone(reportVerdictState(report));
}

/** §2.3's `report 01J9…8Q11 ⧉ · test-set a41c… · finalized`. */
function ReportVerdict(props: { report: Report }): JSX.Element {
  return (
    <VerdictBar
      tone={reportVerdictTone(props.report)}
      verdict={reportVerdictLine(props.report)}
      identity={
        <span class="report-identity">
          report <CopyId id={props.report.reportId} label="report" /> · test-set{" "}
          <CopyId id={props.report.testSetHash} label="test-set" /> · {props.report.state}
        </span>
      }
    />
  );
}

function ReportFound(props: {
  report: Report;
  refresh: JSX.Element;
  onRefresh: () => void;
}): JSX.Element {
  /*
   * What the palette can reach on this screen (§6 step 7). One section, so one
   * anchor — §2.0's tree draws exactly `cases` under a report — and the refresh
   * action is the screen's own callback, so the palette and the visible control
   * can never mean two different things by it.
   */
  contributeScreen(() => ({
    anchors: [{ id: "report-cases", label: "cases" }],
    actions: [{ id: "refresh", label: "refresh this screen", run: props.onRefresh }],
  }));

  return (
    <>
      <ReportVerdict report={props.report} />
      {/*
       * The reason the verdict line does not carry, said once, below the bar.
       * Only when there is none: a projection that grows one puts it in the line
       * and this note has nothing left to say.
       */}
      <Show when={unstatedPendingReason(props.report)}>
        <p class="report-note frame">
          no reason was recorded for this report being pending — the read carries none
        </p>
      </Show>

      <div class="report-section" id="report-cases" tabindex="-1">
        <SectionLabel label="cases" trailing={props.refresh} />
        <Switch>
          <Match when={finalized(props.report)}>
            {(report) => <ReportCases cases={report().cases} />}
          </Match>
          {/*
           * Not "no cases": a reservation that has not finalized has no case
           * list to be empty, which is a different fact from a set that ran and
           * held nothing.
           */}
          <Match when={pending(props.report)}>
            <EmptyState
              region="cases"
              reason="this report has not finalized, so it records no cases yet"
            />
          </Match>
        </Switch>
      </div>
    </>
  );
}
