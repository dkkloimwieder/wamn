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
import type { ReadResult, ReaderError } from "../reader/errors";
import { selectReader } from "../reader/reader";
import type { ExecutionFact, JsonValue, Run } from "../reader/types";
import { CopyId } from "../ui/copy-id";
import { ErrorPanel } from "../ui/error-panel";
import { FailurePanel } from "../ui/failure-panel";
import { JsonView } from "../ui/json-view";
import { KeyValue } from "../ui/key-value";
import { RefreshControl } from "../ui/refresh-control";
import { SectionLabel } from "../ui/section-label";
import { EmptyState, LoadingBlock } from "../ui/states";
import { UncertainPanel } from "../ui/uncertain-panel";
import { ExecutionTable, factCountSummary } from "./execution-table";
import { FactInspector } from "./fact-inspector";
import { RunVerdict } from "./run-verdict";
import "./run-screen.css";

/**
 * §2.2's run screen: one read of one run, then the verdict before the eye
 * moves and the evidence below it in the order the author asks for it —
 * *did it work → why not → what happened step by step → what did it produce*.
 *
 * The screen owns the read and nothing else. Every part below it takes a `Run`
 * and renders it; this file decides which parts stand, in which order, and what
 * each of the read's three outcomes looks like.
 */
export function RunScreen(props: { id: string }): JSX.Element {
  const reader = selectReader();
  /** §2.6's refresh: re-issuing the screen's one read is bumping this. */
  const [issued, setIssued] = createSignal(0);
  const [refreshedAt, setRefreshedAt] = createSignal<Date | null>(null);
  /*
   * Which read the writes below belong to, as plain values rather than signals:
   * they are consulted after an await, where a signal read tracks nothing and a
   * signal write would land in the memo's owned scope. Their only job is to let
   * a settled read ask whether it is still the one being waited on.
   */
  let latest = 0;
  let disposed = false;
  onCleanup(() => {
    disposed = true;
  });

  const read = createMemo(async () => {
    issued();
    const issue = (latest += 1);
    const result = await reader.runs.get(props.id);
    /*
     * §1.4's status dot is derived from reads, and this is the first screen
     * that issues one. Both writes land after the await, which is what puts
     * them outside the memo's owned scope — and what makes them arrive in
     * whatever order the platform answers in. The dot describes the *last*
     * read, so an older answer may not overwrite a newer one, and a screen the
     * author has already left may not move global state at all.
     */
    if (disposed || issue !== latest) {
      return result;
    }
    setReadStatus(readStatusOf(result));
    setRefreshedAt(new Date());
    return result;
  });

  return (
    <section class="run-screen">
      {/* A fault in a part must not blank the shell around it (§1.4). */}
      <Errored
        fallback={(fault) => (
          <p class="run-fault frame">the run screen failed to render: {String(fault())}</p>
        )}
      >
        <Loading fallback={<LoadingBlock region="the run" />}>
          <RunBody
            read={read}
            refresh={
              <RefreshControl
                refreshedAt={refreshedAt()}
                busy={isPending(read)}
                onRefresh={() => setIssued((count) => count + 1)}
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
 * (`reader/errors.ts`: a missing resource is not a transport error) and the
 * screen goes on to show it, so mistyping a run id must not tell the author the
 * console cannot reach the target. Every other refusal is a read the author did
 * not get an answer to.
 */
function readStatusOf(result: ReadResult<Run>): ReadStatus {
  return result.ok || result.error.kind === "not-found" ? "ok" : "fail";
}

/** The reader never throws, so both outcomes of a settled read are values here. */
function readRun(result: ReadResult<Run>): Run | null {
  return result.ok ? result.value : null;
}

function readRefusal(result: ReadResult<Run>): ReaderError | null {
  return result.ok ? null : result.error;
}

/**
 * The settled read. Pending is the `Loading` fallback above; a refusal is the
 * typed value the platform answered with, and keeps the refresh control so the
 * one control that could change the answer is still reachable.
 */
function RunBody(props: { read: () => ReadResult<Run>; refresh: JSX.Element }): JSX.Element {
  return (
    <Switch>
      <Match when={readRun(props.read())}>
        {(run) => <RunFound run={run()} refresh={props.refresh} />}
      </Match>
      <Match when={readRefusal(props.read())}>
        {(error) => (
          <div class="run-section">
            <SectionLabel label="run" trailing={props.refresh} />
            <ErrorPanel error={error()} />
          </div>
        )}
      </Match>
    </Switch>
  );
}

function RunFound(props: { run: Run; refresh: JSX.Element }): JSX.Element {
  /**
   * §6 step 4's trace mode, in `RowDisclosure`'s controlled mode: on, it holds
   * every row open and compresses every inspector; off, it hands each row back
   * to the reader — which is what lets someone open one row by hand, read the
   * whole trace, leave it, and still find their row open. `false` is never
   * latched: leaving trace mode releases the rows rather than closing them, so
   * collapse-all closes exactly the rows trace mode opened.
   */
  const [trace, setTrace] = createSignal(false);

  return (
    <>
      <RunVerdict run={props.run} />
      <Switch>
        {/*
         * §2.2, plainly: when the run is effect-uncertain the run-failure
         * section *is replaced by* the uncertain panel — label and all. The
         * panel is the section, and it names its own state at head size.
         */}
        <Match when={props.run.uncertainty}>
          {(uncertainty) => <UncertainPanel uncertainty={uncertainty()} />}
        </Match>
        <Match when={props.run.failure}>
          {(failure) => (
            <div class="run-section">
              <SectionLabel label="run failure" />
              <FailurePanel failure={failure()} />
            </div>
          )}
        </Match>
      </Switch>

      <div class="run-section">
        <SectionLabel
          label="execution"
          trailing={
            <span class="run-controls">
              {/* §2.2's `EXECUTION ──── 14 facts, all`: the count states the
                  read's truthfulness on the rule, not only under the table. */}
              <span class="run-count frame">{factCountSummary(props.run.factCount)}</span>
              {/*
               * Named for what they act on: `expand all` alone would not say
               * what it expands, and two controls on one screen may not share a
               * name. The state is stated as a word rather than as a pressed
               * pair — trace mode holds every row open, so while it is off the
               * table is whatever the reader made it and neither control may
               * claim to describe it.
               */}
              <button
                type="button"
                class="run-control"
                aria-label="expand all execution rows"
                onClick={() => setTrace(true)}
              >
                expand all
              </button>
              <button
                type="button"
                class="run-control"
                aria-label="collapse all execution rows"
                onClick={() => setTrace(false)}
              >
                collapse all
              </button>
              {/*
               * Mounted whether or not it says anything: a live region inserted
               * at the same moment as its text is not reliably announced, and
               * this is the one state on the screen that moves every row at
               * once — the reader who pressed the control hears that it did.
               */}
              <span class="run-trace-state label" aria-live="polite">
                {trace() ? "trace mode" : ""}
              </span>
              {/* §2.6: refresh is a single control, and it sits on a section rule. */}
              {props.refresh}
            </span>
          }
        />
        <ExecutionTable
          run={props.run}
          open={trace() ? true : undefined}
          /*
           * While trace mode holds every row open, a row's own toggle cannot
           * move it — the controlled value outranks the row's state. Rather
           * than leave a control that says `collapse fact 3` doing nothing to
           * fact 3, the row reports the toggle and trace mode ends on it: the
           * row the reader collapsed is the row that collapses, and every other
           * row returns to whatever the reader had made of it.
           */
          onToggle={() => setTrace(false)}
          inspector={(fact) => (
            <FactInspector fact={fact} run={props.run} compressed={trace()} />
          )}
        />
      </div>

      <div class="run-section">
        <SectionLabel label="context" />
        <RunContextSection run={props.run} />
      </div>

      <div class="run-section">
        <SectionLabel label="details" />
        <RunDetails run={props.run} />
      </div>
    </>
  );
}

/**
 * §6 step 4's run-level CONTEXT: what the run's context document ended up
 * being, folded last-wins across the returned facts.
 *
 * The mode is a property of the run — every fact carries the same one — so it
 * is read from the run once and only `before-write-after` has anything to fold.
 * `final-only` is a single document for the whole run, and `absent` is the
 * truthful mode today: no per-node context exists durably (STEP-9 RISK).
 */
export type RunContext =
  | { readonly mode: "absent" }
  | { readonly mode: "final-only"; readonly final: JsonValue }
  | {
      readonly mode: "before-write-after";
      /** The last write, or the document the returned facts began with. */
      readonly final: JsonValue;
      /** The facts that replaced the document, in returned order. */
      readonly writes: readonly ExecutionFact[];
    };

export function foldRunContext(run: Run): RunContext {
  if (run.facts.length === 0) {
    return { mode: "absent" };
  }
  const lane = run.facts[0].context;
  if (lane.mode === "absent") {
    return { mode: "absent" };
  }
  if (lane.mode === "final-only") {
    return { mode: "final-only", final: lane.final };
  }

  // Replacement is whole-document, so the fold is the last write and not a
  // merge of them; a fact in any other mode has nothing foldable to add.
  let final: JsonValue = lane.before;
  const writes: ExecutionFact[] = [];
  for (const fact of run.facts) {
    const wrote = fact.context;
    if (wrote.mode === "before-write-after" && wrote.write !== null) {
      final = wrote.write;
      writes.push(fact);
    }
  }
  return { mode: "before-write-after", final, writes };
}

/** Which facts of the fold moved the document, and which one had the last word. */
function writeSummary(run: Run, writes: readonly ExecutionFact[]): string {
  if (writes.length === 0) {
    // A truncated read never saw the run's opening document, only its window's.
    return run.factCount.truncated
      ? "no returned fact replaced it, so it is the document the returned facts began with"
      : "no returned fact replaced it, so it is the document the run began with";
  }
  const last = writes[writes.length - 1];
  return `${writes.length} of ${run.facts.length} returned facts replaced it · last at fact ${last.index}, ${last.node}`;
}

function RunContextSection(props: { run: Run }): JSX.Element {
  const folded = createMemo(() => foldRunContext(props.run));
  /** Bound once inside each closure: two calls of one accessor narrow nothing. */
  const single = () => {
    const context = folded();
    return context.mode === "final-only" ? context : null;
  };
  const chain = () => {
    const context = folded();
    return context.mode === "before-write-after" ? context : null;
  };

  return (
    <Switch>
      <Match when={chain()}>
        {(context) => (
          <>
            <KeyValue label="final">
              <JsonView value={context().final} subject="the run's final context" />
            </KeyValue>
            <KeyValue label="writes">{writeSummary(props.run, context().writes)}</KeyValue>
            {/* A truncated read folds the facts it was given, and says so. */}
            <Show when={props.run.factCount.truncated}>
              <p class="run-note frame">
                folded across the returned facts only — this read was truncated
              </p>
            </Show>
          </>
        )}
      </Match>
      <Match when={single()}>
        {(context) => (
          <>
            <p class="run-note frame">
              one document for the whole run: there is nothing to fold, and it says nothing about
              what any single node did
            </p>
            <KeyValue label="final">
              <JsonView value={context().final} subject="the run's final context" />
            </KeyValue>
          </>
        )}
      </Match>
      {/* Not "the context was empty": nothing was captured, so nothing can be. */}
      <Match when={folded().mode === "absent"}>
        <EmptyState region="context" reason="this run captured no context" />
      </Match>
    </Switch>
  );
}

/**
 * The run-level plan hash. `RunProjection` carries none — the hash is a
 * `node_runs` column — so it is the returned facts' own, and only when they
 * agree on one. Naming the first fact's hash as the run's would be a claim.
 */
export function sharedPlanHash(run: Run): string | null {
  if (run.facts.length === 0) {
    return null;
  }
  const first = run.facts[0].planHash;
  return run.facts.every((fact) => fact.planHash === first) ? first : null;
}

/**
 * §6 step 4's DETAILS: the run-level facts that are not the verdict.
 *
 * The ids the verdict bar carries are printed whole here — the bar shows them
 * middle-ellipsized — and the copy affordance stays where §2.6 puts it, once
 * per id, on that line. The plan hash is the one id the bar does not carry, so
 * it is the one that copies from here.
 */
function RunDetails(props: { run: Run }): JSX.Element {
  return (
    <>
      <KeyValue label="trace">{props.run.traceId}</KeyValue>
      <KeyValue label="plan hash">
        <Show
          when={sharedPlanHash(props.run)}
          fallback={
            // Two different absences, and one wording for each: a read that
            // returned nothing has no hash to agree on in the first place.
            <span class="run-absent frame">
              {props.run.facts.length === 0
                ? "this read returned no facts, so it carries no plan hash"
                : "no single plan hash across the returned facts"}
            </span>
          }
        >
          {(hash) => (
            <>
              <CopyId id={hash()} label="plan hash" />
              {/* CONTEXT's scruple, kept here: agreement across a window is not
                  agreement across the run. */}
              <Show when={props.run.factCount.truncated}>
                <span class="run-note frame"> · agreed by the returned facts only</span>
              </Show>
            </>
          )}
        </Show>
      </KeyValue>
      <KeyValue label="capture">{props.run.capture}</KeyValue>
      {/* One spelling of the draft on the screen, and it is the verdict bar's. */}
      <KeyValue label="draft">
        {props.run.draft.flowId}@{props.run.draft.revision}
      </KeyValue>
      <KeyValue label="facts">{factCountSummary(props.run.factCount)}</KeyValue>
    </>
  );
}
