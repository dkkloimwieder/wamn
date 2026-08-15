import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import type { JSX } from "@solidjs/web";
import { For, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import type { ReadResult, ReaderError } from "../reader/errors";
import { fixtureReader } from "../reader/fixture-reader";
import {
  effectUncertainRun,
  failingRun,
  finalizedReport,
  nonParsingDraft,
} from "../reader/fixtures";
import type { ExecutionFact, ReportCase } from "../reader/types";
import { CopyId } from "./copy-id";
import { DataTable, type Column, type RowDisclosure } from "./data-table";
import { Disclosure } from "./disclosure";
import { ErrorPanel } from "./error-panel";
import { FailurePanel } from "./failure-panel";
import { JsonView } from "./json-view";
import { KeyValue } from "./key-value";
import { PassGlyph } from "./pass-glyph";
import { RefreshControl } from "./refresh-control";
import { SectionLabel } from "./section-label";
import { SourceView } from "./source-view";
import { nodeRunTone, runTone } from "./status";
import { StatusBadge } from "./status-badge";
import { EmptyState, LoadingBlock } from "./states";
import { VerdictBar } from "./verdict-bar";

/**
 * §3's accessibility floor, checked here once for every step-3 primitive at
 * once. Each assertion walks the rendered tree rather than naming an element,
 * so it holds for whatever the primitives become — and every rendering below is
 * driven by a step-2 fixture, in the state the screen that uses it produces.
 */

afterEach(cleanup);

// ── the primitives, each in a state its screen renders ──────────────────────

/** The failing run's failure, which §2.2's panel and verdict bar are both built from. */
const failure = failingRun.failure;
if (failure === null) {
  throw new Error("the failing run fixture must carry a run failure");
}

const reportCases: readonly ReportCase[] =
  finalizedReport.state === "finalized" ? finalizedReport.cases : [];

/** §2.4's tinted line, taken from the fixture's own parse verdict, never guessed. */
const draftParse = nonParsingDraft.parse;
const errorLine = draftParse.ok ? null : draftParse.line;

/** Every error state below is one the fixture reader actually refused. */
function refused<T>(result: ReadResult<T>): ReaderError {
  if (result.ok) {
    throw new Error("expected the read to be refused");
  }
  return result.error;
}

const readerErrors: readonly ReaderError[] = [
  refused(await fixtureReader.runs.get("01J9NOTHINGSTOREDHEREXX")),
  refused(await fixtureReader.reports.get("refused-authorization")),
  refused(await fixtureReader.drafts.get("network-down", 1)),
];

const factColumns: ReadonlyArray<Column<ExecutionFact>> = [
  { header: "node", cell: (fact) => fact.node },
  {
    header: "status",
    cell: (fact) => <StatusBadge status={fact.status} tone={nodeRunTone(fact.status)} />,
  },
];

/** §2.2's row disclosure: the captured output, or why there is none. */
const factDisclosure = (fact: ExecutionFact): RowDisclosure => ({
  label: `fact ${fact.index} ${fact.node}`,
  content: () =>
    fact.output.state === "present" ? (
      <JsonView value={fact.output.value} subject={`fact ${fact.index} output`} />
    ) : (
      <EmptyState region="output" reason="nothing was captured for this node" />
    ),
});

const caseColumns: ReadonlyArray<Column<ReportCase>> = [
  { header: "result", cell: (reportCase) => <PassGlyph passed={reportCase.passed} /> },
  { header: "case", cell: (reportCase) => reportCase.caseId },
];

/** §2.3: a failed case arrives open, showing each failed assertion as a pair. */
const caseDisclosure = (reportCase: ReportCase): RowDisclosure | null =>
  reportCase.passed
    ? null
    : {
        label: `case ${reportCase.caseId}`,
        startOpen: true,
        content: () => (
          <For each={reportCase.failedAssertions}>
            {(assertion) => (
              <>
                <KeyValue label="expected">{assertion.expected.family}</KeyValue>
                <KeyValue label="observed">{assertion.observed}</KeyValue>
              </>
            )}
          </For>
        ),
      };

type Primitive = {
  readonly name: string;
  readonly render: () => JSX.Element;
  /**
   * Status words this rendering must say through an element that carries a
   * tone. Empty for the primitives that carry no status at all.
   */
  readonly says: readonly string[];
};

const primitives: readonly Primitive[] = [
  {
    name: "StatusBadge",
    render: () => (
      <>
        <StatusBadge status={failingRun.status} tone={runTone(failingRun.status)} />
        <For each={failingRun.facts}>
          {(fact) => <StatusBadge status={fact.status} tone={nodeRunTone(fact.status)} />}
        </For>
      </>
    ),
    says: ["failed", "success", "error"],
  },
  {
    name: "PassGlyph",
    render: () => (
      <For each={reportCases}>{(reportCase) => <PassGlyph passed={reportCase.passed} />}</For>
    ),
    says: ["passed", "failed"],
  },
  {
    name: "VerdictBar",
    render: () => (
      <>
        <VerdictBar
          tone={runTone(failingRun.status)}
          verdict={`${failingRun.status.toUpperCase()} · ${failure.kind} at ${failure.node}`}
          identity={<CopyId id={failingRun.runId} label="run" />}
        />
        {/* the one state §2.2 exists to stop an author misreading, so the floor holds it too */}
        <VerdictBar
          tone={runTone(effectUncertainRun.status)}
          verdict={effectUncertainRun.status.toUpperCase()}
          identity={<CopyId id={effectUncertainRun.traceId} label="trace" />}
        />
      </>
    ),
    says: ["fail", "FAILED", "uncertain", "EFFECT-UNCERTAIN"],
  },
  {
    name: "SectionLabel · RefreshControl",
    render: () => (
      <SectionLabel
        label="execution"
        trailing={
          <RefreshControl
            refreshedAt={new Date("2026-08-14T12:04:31Z")}
            onRefresh={() => undefined}
          />
        }
      />
    ),
    says: [],
  },
  {
    name: "KeyValue",
    render: () => <KeyValue label="trace">{failingRun.traceId}</KeyValue>,
    says: [],
  },
  {
    name: "CopyId",
    render: () => (
      <>
        <CopyId id={failingRun.runId} label="run" />
        <CopyId id={failingRun.traceId} label="trace" />
      </>
    ),
    says: [],
  },
  {
    name: "Disclosure",
    render: () => (
      <Disclosure label="raw failure">
        <JsonView value={failure.raw} subject="the raw failure" />
      </Disclosure>
    ),
    says: [],
  },
  {
    name: "DataTable · execution",
    render: () => (
      <DataTable
        caption="execution facts"
        columns={factColumns}
        rows={failingRun.facts}
        rowKey={(fact) => String(fact.index)}
        tone={(fact) => (fact.status === "error" ? "fail" : null)}
        disclosure={factDisclosure}
        empty={<EmptyState region="execution" reason="this read returned no facts" />}
        footer={`${failingRun.factCount.returned} facts, all`}
      />
    ),
    says: ["error", "success"],
  },
  {
    name: "DataTable · cases",
    render: () => (
      <DataTable
        caption="test cases"
        columns={caseColumns}
        rows={reportCases}
        rowKey={(reportCase) => reportCase.caseId}
        disclosure={caseDisclosure}
        empty={<EmptyState region="cases" reason="this report finalized with no cases" />}
      />
    ),
    says: ["passed", "failed"],
  },
  {
    name: "JsonView",
    render: () => <JsonView value={failure.raw} subject="the raw failure" />,
    says: [],
  },
  {
    name: "SourceView",
    render: () => <SourceView text={nonParsingDraft.definition} errorLine={errorLine} />,
    says: [`parse error on line ${errorLine}`],
  },
  {
    name: "FailurePanel",
    render: () => <FailurePanel failure={failure} />,
    says: [],
  },
  {
    name: "ErrorPanel",
    render: () => <For each={readerErrors}>{(error) => <ErrorPanel error={error} />}</For>,
    says: ["not found", "refused", "no response"],
  },
  {
    name: "LoadingBlock · EmptyState",
    render: () => (
      <>
        <LoadingBlock region="execution" />
        <EmptyState region="output" reason="capture was off for this run" />
      </>
    ),
    says: [],
  },
];

// ── what a screen reader reaches ────────────────────────────────────────────

/**
 * What a reader would reach on this element: its label if it has one, otherwise
 * its text with every aria-hidden subtree removed. jsdom computes no accessible
 * name, so this is the floor's approximation of one — strict on purpose, since
 * markup a reader never announces must not be able to satisfy an assertion here.
 */
function reachableText(element: Element): string {
  if (element.closest('[aria-hidden="true"]') !== null) {
    return "";
  }
  const label = element.getAttribute("aria-label");
  if (label !== null) {
    return label.trim();
  }
  const copy = element.cloneNode(true) as Element;
  for (const hidden of copy.querySelectorAll('[aria-hidden="true"]')) {
    hidden.remove();
  }
  return (copy.textContent ?? "").replace(/\s+/g, " ").trim();
}

/**
 * Opens everything that opens, so the floor also sees content that only exists
 * once disclosed. JsonView's children carry expanders of their own, so this
 * repeats; the cap turns a disclosure that reopens itself into a failure rather
 * than a hung suite.
 */
function expandAll(container: HTMLElement): void {
  for (let pass = 0; pass < 8; pass += 1) {
    const closed = [...container.querySelectorAll<HTMLElement>('button[aria-expanded="false"]')];
    if (closed.length === 0) {
      return;
    }
    for (const button of closed) {
      fireEvent.click(button);
    }
    flush();
  }
  expect(container.querySelector('button[aria-expanded="false"]')).toBeNull();
}

/** Every primitive, closed and then opened, because a floor holds in both states. */
function eachRendering(check: (primitive: Primitive, container: HTMLElement) => void): void {
  for (const primitive of primitives) {
    const { container } = render(primitive.render);
    check(primitive, container);
    expandAll(container);
    check(primitive, container);
    cleanup();
  }
}

describe("accessibility floor", () => {
  it("never leaves a tone to carry a status on its own", () => {
    const offences: string[] = [];
    eachRendering((primitive, container) => {
      const toned = [...container.querySelectorAll("[data-tone]")];
      const spoken = toned.map(reachableText);
      for (const [index, element] of toned.entries()) {
        if (!/\p{L}/u.test(spoken[index])) {
          const tone = element.getAttribute("data-tone");
          offences.push(`${primitive.name}: [data-tone=${tone}] says nothing a reader reaches`);
        }
      }
      for (const word of primitive.says) {
        if (!spoken.some((said) => said.includes(word))) {
          offences.push(`${primitive.name}: no toned element says "${word}"`);
        }
      }
    });
    expect(offences).toEqual([]);
  });

  it("puts every interactive control in the tab order, as a button", () => {
    const offences: string[] = [];
    let handlers = 0;
    eachRendering((primitive, container) => {
      for (const element of container.querySelectorAll<HTMLElement>("*")) {
        // Solid lands a delegated handler on the node itself, as `$$click`. A
        // listener added any other way is invisible from here — that is bead
        // wamn-dggp.16's to see, in a real browser.
        const handled = Object.keys(element).some((key) => key.startsWith("$$"));
        const where = `${primitive.name}: <${element.tagName.toLowerCase()}>`;
        if (handled) {
          handlers += 1;
          if (element.tagName !== "BUTTON") {
            offences.push(`${where} handles events but is not a button`);
          }
        }
        if (element.getAttribute("role") === "button" && element.tagName !== "BUTTON") {
          offences.push(`${where} plays a button instead of being one`);
        }
        if ((element.getAttribute("tabindex") ?? "").startsWith("-")) {
          offences.push(`${where} is out of the tab order`);
        }
      }
    });
    expect(offences).toEqual([]);
    // the scan is worth nothing if it never sees a handler at all
    expect(handlers).toBeGreaterThan(0);
  });

  it("names every button by what it acts on", () => {
    const offences: string[] = [];
    eachRendering((primitive, container) => {
      const named = new Map<string, number>();
      for (const button of container.querySelectorAll("button")) {
        const name = reachableText(button);
        if (!/\p{L}/u.test(name)) {
          offences.push(`${primitive.name}: a button is named ${JSON.stringify(name)}`);
        }
        named.set(name, (named.get(name) ?? 0) + 1);
      }
      // Two controls of one screen act on two different things, so a name that
      // repeats is a name that stopped saying which — the shape a dropped label
      // takes when every toggle falls back to "expand".
      for (const [name, count] of named) {
        if (count > 1) {
          const spelling = JSON.stringify(name);
          offences.push(`${primitive.name}: ${count} buttons are all named ${spelling}`);
        }
      }
    });
    expect(offences).toEqual([]);
  });

  it("captions every table", () => {
    const offences: string[] = [];
    eachRendering((primitive, container) => {
      for (const table of container.querySelectorAll("table")) {
        const caption = table.querySelector(":scope > caption");
        if (caption === null || !/\p{L}/u.test(reachableText(caption))) {
          offences.push(`${primitive.name}: a table carries no caption`);
        }
      }
    });
    expect(offences).toEqual([]);
  });

  // ── the two the DOM cannot answer ──────────────────────────────────────────

  /**
   * jsdom parses no stylesheet, so a ring erased in CSS is invisible to every
   * render above: the last two are source lints, over the bytes rather than
   * over an import — vitest hands a stylesheet back as an empty module, `?raw`
   * included. Needles are spelled with a throwaway character class so the scan
   * does not trip over its own source.
   *
   * The directory is taken whole rather than resolved against `import.meta.url`:
   * under jsdom a relative URL reference resolves against the document, which is
   * an http: URL that names no path.
   */
  const uiDirectory = import.meta.dirname;
  const uiSources = readdirSync(uiDirectory).map(
    (name) => [name, readFileSync(join(uiDirectory, name), "utf8")] as const,
  );

  it("leaves the one focus ring to app.css", () => {
    const ring = readFileSync(join(uiDirectory, "..", "styles", "app.css"), "utf8");
    // the ring the whole package depends on, declared once and nowhere else
    expect(ring.match(/:focus-[v]isible/g)).toHaveLength(1);

    const offences: string[] = [];
    for (const [name, source] of uiSources) {
      if (/:focus-[v]isible/.test(source)) {
        offences.push(`${name}: declares a focus ring of its own`);
      }
      if (/\b[o]utline\s*:\s*(?:none|0)\b/.test(source)) {
        offences.push(`${name}: erases the focus ring`);
      }
    }
    expect(offences).toEqual([]);
  });

  it("routes every transition through --motion-fast", () => {
    const declaration = /\b(?:[t]ransition|animation)(?:-duration|-delay)?\s*:\s*([^;{]+)/g;
    const offences: string[] = [];
    for (const [name, source] of uiSources) {
      for (const [, value] of source.matchAll(declaration)) {
        // tokens.css drops motion under prefers-reduced-motion for the whole
        // package, but a hard-coded duration is still motion nothing tuned.
        if (!value.includes("var(--motion-fast)") && value.trim() !== "none") {
          offences.push(`${name}: ${value.trim()} does not reach for --motion-fast`);
        }
      }
    }
    expect(offences).toEqual([]);
  });
});
