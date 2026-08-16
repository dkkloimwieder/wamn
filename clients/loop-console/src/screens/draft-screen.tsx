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
import type { CommitProvenance, Draft } from "../reader/types";
import { recordVisit, visitFromDraft } from "../store/visited";
import { CopyId } from "../ui/copy-id";
import { ErrorPanel } from "../ui/error-panel";
import { KeyValue } from "../ui/key-value";
import { RefreshControl } from "../ui/refresh-control";
import { SectionLabel } from "../ui/section-label";
import { SourceView } from "../ui/source-view";
import { EmptyState, LoadingBlock } from "../ui/states";
import { StatusBadge } from "../ui/status-badge";
import { VerdictBar } from "../ui/verdict-bar";
import { DraftStructure } from "./draft-structure";
import "./draft-screen.css";

/**
 * §2.4's draft screen: one read of one revision, then *what exactly is stored* —
 * and, if it parses, the shape those bytes describe.
 *
 * The read is `run-screen.tsx`'s, resource for resource: one memo, one stale-read
 * guard, one refresh, and the same rule about which read may move global state.
 * What is this screen's own is the pair of tabs, and their order is the design
 * stating the contract's truth: the bytes are the draft, so the bytes are what
 * stands first and by default. The structure is a reading of them.
 */

/** §1's grouped digits (`4,812`), pinned to one locale so a render never depends on the machine's. */
const digits = new Intl.NumberFormat("en-US");

type Tab = "source" | "structure";

/** The panel ids §2.6's palette lands on; one of them is mounted at a time. */
const panelIds: Record<Tab, string> = {
  source: "draft-source",
  structure: "draft-structure",
};

export function DraftScreen(props: { id: string; revision: number }): JSX.Element {
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
    const result = await reader.drafts.get(props.id, props.revision);
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
    // §6's "4–5 write it": only a draft that came back — a refusal names no
    // entity to remember, and §2.0's tree addresses a draft by both id and
    // revision, which is why the entry carries the revision that was read.
    if (result.ok) {
      recordVisit(visitFromDraft(result.value, Date.now()));
    }
    return result;
  });

  const refresh = (): void => {
    setIssued((count) => count + 1);
  };

  return (
    <section class="draft-screen">
      {/* A fault in a part must not blank the shell around it (§1.4). */}
      <Errored
        fallback={(fault) => (
          <p class="draft-fault frame">the draft screen failed to render: {String(fault())}</p>
        )}
      >
        <Loading fallback={<LoadingBlock region="the draft" />}>
          <DraftBody
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
 * §1.4's dot, from one read. A `not-found` is a 2xx answer carrying a literal, so
 * asking for a revision the platform never stored must not tell the author the
 * console cannot reach the target.
 */
function readStatusOf(result: ReadResult<Draft>): ReadStatus {
  return result.ok || result.error.kind === "not-found" ? "ok" : "fail";
}

/** The reader never throws, so both outcomes of a settled read are values here. */
function readDraft(result: ReadResult<Draft>): Draft | null {
  return result.ok ? result.value : null;
}

function readRefusal(result: ReadResult<Draft>): ReaderError | null {
  return result.ok ? null : result.error;
}

function DraftBody(props: {
  read: () => ReadResult<Draft>;
  refresh: JSX.Element;
  onRefresh: () => void;
}): JSX.Element {
  return (
    <Switch>
      <Match when={readDraft(props.read())}>
        {(draft) => (
          <DraftFound draft={draft()} refresh={props.refresh} onRefresh={props.onRefresh} />
        )}
      </Match>
      <Match when={readRefusal(props.read())}>
        {(error) => (
          <DraftRefused error={error()} refresh={props.refresh} onRefresh={props.onRefresh} />
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
function DraftRefused(props: {
  error: ReaderError;
  refresh: JSX.Element;
  onRefresh: () => void;
}): JSX.Element {
  contributeScreen(() => ({
    anchors: [],
    actions: [{ id: "refresh", label: "refresh this screen", run: props.onRefresh }],
  }));

  return (
    <div class="draft-section">
      <SectionLabel label="draft" trailing={props.refresh} />
      <ErrorPanel error={props.error} />
    </div>
  );
}

/**
 * §2.4's `saved 2026-08-14 09:12`, with the zone it is in named.
 *
 * Rendered from the instant's UTC parts rather than from the reader's clock: a
 * stored timestamp printed with no zone beside it is a claim about which clock
 * it was taken on, and this one is not the browser's. A value this console
 * cannot read as an instant is printed exactly as it arrived — `savedAt` has no
 * read-side source at all today (STEP-9 RISK), so its format is the platform's
 * to decide and not this screen's to assume.
 */
export function savedAtLine(savedAt: string): string {
  const at = new Date(savedAt);
  if (Number.isNaN(at.getTime())) {
    return savedAt;
  }
  const iso = at.toISOString();
  return `${iso.slice(0, 10)} ${iso.slice(11, 16)} UTC`;
}

/** §2.4's `does not parse — line 41: …`, and the line-less shapes it also has to read. */
export function parseRefusal(draft: Draft): string | null {
  const parse = draft.parse;
  if (parse.ok) {
    return null;
  }
  // `DraftParse.line` is null on several real engine shapes; a label that
  // guessed one would send the reader to a line the error is not on.
  return parse.line === null
    ? `does not parse — ${parse.message}`
    : `does not parse — line ${parse.line}: ${parse.message}`;
}

function DraftFound(props: {
  draft: Draft;
  refresh: JSX.Element;
  onRefresh: () => void;
}): JSX.Element {
  const [asked, setAsked] = createSignal<Tab>("source");
  /*
   * What the reader asked for is not always what can stand: a refresh that lands
   * a revision which stopped parsing falls back to the bytes rather than leaving
   * a structure tab open over a document there is no structure for.
   */
  const showing = (): Tab =>
    asked() === "structure" && props.draft.parse.ok ? "structure" : "source";
  const refusal = () => parseRefusal(props.draft);
  /** The line the parse failed on, from the read's own verdict and never guessed. */
  const errorLine = () => (props.draft.parse.ok ? null : props.draft.parse.line);

  /*
   * What the palette can reach on this screen (§6 step 7), read live inside the
   * thunk. One tab stands at a time, so the anchor is the panel that is actually
   * mounted: an anchor for the other tab would scroll a reader to nothing. Which
   * is why switching tabs is offered as an action instead — and the structure
   * action only while the draft parses, because that is the only state the
   * structure panel can be reached in at all.
   */
  contributeScreen(() => ({
    anchors: [{ id: panelIds[showing()], label: showing() }],
    actions: [
      { id: "show-source", label: "show this draft's source", run: () => setAsked("source") },
      ...(props.draft.parse.ok
        ? [
            {
              id: "show-structure",
              label: "show this draft's structure",
              run: () => setAsked("structure"),
            },
          ]
        : []),
      { id: "refresh", label: "refresh this screen", run: props.onRefresh },
    ],
  }));

  return (
    <>
      <VerdictBar
        tone="neutral"
        // §2.4's own drawing of the line. A draft has no verdict — §2.0's `•` —
        // so the bar states what this screen is looking at instead of a state
        // it would have to invent one of.
        verdict={`DRAFT ${props.draft.flowId} @ ${props.draft.revision}`}
        identity={
          <span class="draft-head">
            <Show
              when={props.draft.savedAt}
              fallback={
                // Not an empty date, not "never saved", and not "no save time
                // recorded" either: `read-draft` has no source for `savedAt` at
                // all (types.ts STEP-9 RISK), so the absence belongs to this
                // read. Saying it belongs to the draft would be a claim about
                // the platform's records that this console never saw.
                <span class="draft-absent">this read carries no save time</span>
              }
            >
              {(savedAt) => <span>saved {savedAtLine(savedAt())}</span>}
            </Show>{" "}
            · {digits.format(props.draft.byteLength)} bytes · draft{" "}
            <CopyId id={props.draft.draftId} label="draft" /> ·{" "}
            <Tabs showing={showing()} onShow={setAsked} refusal={refusal()} />
          </span>
        }
      />

      <DraftProvenance provenance={props.draft.provenance} />

      {/*
       * One section, whichever tab stands in it: the label, the refresh control
       * on its rule (§2.6), and the panel's own id so the palette lands on the
       * panel that is there.
       */}
      <div class="draft-section" id={panelIds[showing()]} tabindex="-1">
        <SectionLabel label={showing()} trailing={props.refresh} />
        <Switch>
          <Match when={showing() === "source"}>
            {/* The exact stored bytes, never reassembled from anything else. */}
            <SourceView text={props.draft.definition} errorLine={errorLine()} />
          </Match>
          <Match when={showing() === "structure"}>
            <DraftStructure definition={props.draft.definition} />
          </Match>
        </Switch>
      </div>
    </>
  );
}

/**
 * §2.4's two tabs. Buttons, not an ARIA tablist: the tablist pattern moves
 * focus with the arrow keys and takes the unselected tab out of the tab order to
 * do it, which is exactly what §3's floor forbids. Two toggle buttons say the
 * same thing — `aria-pressed` carries which one is showing — and both stay
 * reachable by Tab.
 */
function Tabs(props: {
  showing: Tab;
  onShow: (tab: Tab) => void;
  /** Why the structure tab cannot be pressed, or null when it can. */
  refusal: string | null;
}): JSX.Element {
  return (
    <span class="draft-tabs">
      <span aria-hidden="true">[</span>
      <button
        type="button"
        class="draft-tab"
        // spelled out: @solidjs/web drops a `false` aria value entirely
        aria-pressed={props.showing === "source" ? "true" : "false"}
        aria-label="show this draft's source"
        onClick={() => props.onShow("source")}
      >
        source
      </button>
      <span aria-hidden="true">|</span>
      <button
        type="button"
        class="draft-tab"
        aria-pressed={props.showing === "structure" ? "true" : "false"}
        aria-label="show this draft's structure"
        // §2.4: the tab is disabled when there is no structure to show. A
        // disabled control cannot be focused, so — RefreshControl's precedent —
        // the state it is in is stated beside it rather than only on it.
        disabled={props.refusal !== null}
        onClick={() => props.onShow("structure")}
      >
        structure
      </button>
      <span aria-hidden="true">]</span>
      <Show when={props.refusal}>
        {(refusal) => <span class="draft-tab-refusal frame">{refusal()}</span>}
      </Show>
    </span>
  );
}

/**
 * §6 step 5's provenance block.
 *
 * Every word here is the *client's* claim about its own working tree at save
 * time: the platform never runs git, never verified any of it, and (STEP-9 RISK)
 * does not carry it on the draft row at all. So the block says whose claim it is,
 * and says what `dirty` means — content *descended from* that commit rather than
 * equal to it — where an author reading `dirty` beside a commit would otherwise
 * take the commit for something that reproduces these bytes.
 */
function DraftProvenance(props: { provenance: CommitProvenance | null }): JSX.Element {
  return (
    <div class="draft-section">
      <SectionLabel label="provenance" />
      <Show
        when={props.provenance}
        fallback={
          // What is missing is this read's, not the draft's: provenance is
          // recorded with the authoring command and not on the draft row, so a
          // read that carries none is no evidence that none was attached — and
          // an author who sent one from the CLI must not read this as it having
          // been dropped.
          <EmptyState
            region="provenance"
            reason="this read carries no provenance — it is recorded with the authoring command, not on the draft row — so nothing here says which working tree this content came from"
          />
        }
      >
        {(provenance) => (
          <>
            <KeyValue label="commit">
              <CopyId id={provenance().commit} label="commit" />
            </KeyValue>
            <KeyValue label="ref">
              <Show
                when={provenance().ref}
                fallback={<span class="draft-absent">no ref recorded</span>}
              >
                {(ref) => <span>{ref()}</span>}
              </Show>
            </KeyValue>
            <KeyValue label="working tree">
              {/* The word carries the state; §2.4 tones a dirty tree `warn`. */}
              <StatusBadge
                status={provenance().dirty ? "dirty" : "clean"}
                tone={provenance().dirty ? "warn" : "neutral"}
              />
            </KeyValue>
            <p class="draft-note frame">
              the client's own claim at save time — the platform never runs git, and verified none
              of it.
            </p>
            <Show when={provenance().dirty}>
              <p class="draft-note frame">
                dirty means the content is descended from this commit, not equal to it: checking
                this commit out will not reproduce these bytes.
              </p>
            </Show>
          </>
        )}
      </Show>
    </div>
  );
}
