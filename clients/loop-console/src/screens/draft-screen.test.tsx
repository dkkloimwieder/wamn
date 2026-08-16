import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { readStatus, setReadStatus } from "../app/read-status";
import { screenContribution } from "../app/screen-actions";
import { byteLength, parseDraft } from "../reader/draft-text";
import { err, ok, type ReadResult } from "../reader/errors";
import { fixtureReader } from "../reader/fixture-reader";
import {
  draftFixtures,
  draftKey,
  loopingDraft,
  nonParsingDraft,
  parsingDraft,
  unattributedDraft,
} from "../reader/fixtures";
import type { Draft } from "../reader/types";
import { clearVisited, visitedOfKind } from "../store/visited";
import { DraftScreen, parseRefusal, savedAtLine } from "./draft-screen";

afterEach(() => {
  cleanup();
  // both are global state this screen drives, so each test starts from cold
  setReadStatus("never-contacted");
  clearVisited();
  // the stale-read tests hold the fixture reader open; nothing may leak forward
  vi.restoreAllMocks();
});

/**
 * A draft that parses and is not a flow, registered with the reader for the
 * length of this file. `parse.ok` says the bytes are valid JSON and nothing
 * more, and no fixture holds that state — the screen takes an id and reads
 * through `selectReader()`, so a state with no fixture is reached by giving the
 * fixture reader one.
 */
const HALF_FINISHED_ID = "half-finished";
const halfFinishedSource = `{
  "name": "half-finished",
  "version": 1
}
`;
const halfFinishedDraft: Draft = {
  draftId: HALF_FINISHED_ID,
  flowId: "half-finished",
  revision: 1,
  savedAt: "2026-08-15T11:30:00Z",
  byteLength: byteLength(halfFinishedSource),
  definition: halfFinishedSource,
  parse: parseDraft(halfFinishedSource),
  provenance: null,
};

/**
 * A value a part cannot render, so the screen's fault boundary is the one thing
 * under test. Every other field is §2.4's own draft's, and only the bytes throw:
 * the read landed, and it is the source view over them that cannot be drawn.
 */
const FAULT_DRAFT_ID = "fault-while-rendering";
const faultDraft: Draft = {
  ...parsingDraft,
  draftId: FAULT_DRAFT_ID,
  flowId: FAULT_DRAFT_ID,
  revision: 1,
  get definition(): string {
    throw new Error("the stored bytes could not be read");
  },
};

const registry = draftFixtures as Map<string, Draft>;

beforeAll(() => {
  registry.set(draftKey(HALF_FINISHED_ID, 1), halfFinishedDraft);
  registry.set(draftKey(FAULT_DRAFT_ID, 1), faultDraft);
});

afterAll(() => {
  registry.delete(draftKey(HALF_FINISHED_ID, 1));
  registry.delete(draftKey(FAULT_DRAFT_ID, 1));
});

/** Two ticks settle the reader's promise; `flush` lands the reactive work. */
async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  flush();
}

async function screen(id: string, revision: number) {
  const rendered = render(() => <DraftScreen id={id} revision={revision} />);
  await settle();
  return rendered;
}

/** §2.4's line under the verdict, normalized the way a reader reads it. */
const head = (container: HTMLElement): string =>
  (container.querySelector(".draft-head")?.textContent ?? "").replace(/\s+/g, " ").trim();

/** The bytes the source tab is showing, reassembled from the numbered lines. */
const shownSource = (container: HTMLElement): string =>
  [...container.querySelectorAll(".source-line .source-text")]
    .map((line) => line.textContent ?? "")
    .join("\n");

/** The section label that opens the standing panel — `source` or `structure`. */
const panelLabel = (container: HTMLElement): string =>
  container.querySelector(".draft-section[tabindex] .section-label-text")?.textContent ?? "";

/**
 * What a reader reaches on a control: its label if it has one, otherwise its
 * text with every aria-hidden subtree removed. Spelled exactly as
 * `src/ui/accessibility-floor.test.tsx` spells it, so a pair of controls this
 * suite calls distinct cannot fail the floor once the floor reaches this screen.
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

/** One KeyValue's value, by its key — the pairs render as `dt`/`dd`, unspaced. */
function pair(root: HTMLElement, label: string): string {
  const found = [...root.querySelectorAll<HTMLElement>(".key-value")].find(
    (candidate) => candidate.querySelector(".key-value-label")?.textContent === label,
  );
  return found?.querySelector(".key-value-value")?.textContent ?? "";
}

describe("savedAtLine", () => {
  it("names the zone it renders the instant in", () => {
    expect(savedAtLine("2026-08-14T09:12:00Z")).toBe("2026-08-14 09:12 UTC");
  });

  it("prints a value it cannot read as an instant exactly as it arrived", () => {
    // `savedAt` has no read-side source today, so its format is the platform's
    // to decide: a value this console cannot parse is quoted, never dropped
    expect(savedAtLine("yesterday")).toBe("yesterday");
  });
});

describe("parseRefusal", () => {
  it("names the line when the engine reported one", () => {
    expect(parseRefusal(nonParsingDraft)).toBe(
      "does not parse — line 41: Expected ':' after property name",
    );
  });

  it("reads the message alone when it did not, and nothing at all when it parses", () => {
    const lineless: Draft = {
      ...nonParsingDraft,
      parse: { ok: false, line: null, message: "Unexpected end of JSON input" },
    };
    expect(parseRefusal(lineless)).toBe("does not parse — Unexpected end of JSON input");
    expect(parseRefusal(parsingDraft)).toBeNull();
  });
});

describe("DraftScreen — the read", () => {
  it("stands on the loading block until the read settles", async () => {
    const { container } = render(() => <DraftScreen id="orders" revision={17} />);

    expect(container).toHaveTextContent("loading the draft");
    expect(container.querySelector("h1")).toBeNull();

    await settle();
    expect(container).not.toHaveTextContent("loading the draft");
    expect(container.querySelector("h1")).toHaveTextContent("DRAFT orders @ 17");
  });

  it("renders a refusal as the typed value it is, and keeps refresh reachable", async () => {
    const { container, getByRole } = await screen("network-down", 17);

    expect(container.querySelector(".error-panel")).toHaveTextContent("no response");
    // nothing was read, so nothing on the screen may claim to be a verdict
    expect(container.querySelector("h1")).toBeNull();
    getByRole("button", { name: /refresh this screen/ });
    // the transport never answered, so §1.4's dot says the console is out of touch
    expect(readStatus()).toBe("fail");
  });

  it("shows a revision the platform does not hold as the answer it is", async () => {
    const { container } = await screen("orders", 4);

    // the platform's literal, never rewritten, and the revision it was asked for
    expect(container).toHaveTextContent("draft-revision-not-found");
    expect(pair(container, "revision")).toBe("4");
    // a 2xx refusal is an answer: mistyping a revision may not report the
    // console as unable to reach the platform
    expect(readStatus()).toBe("ok");
  });

  it("drives §1.4's dot: a refusal the platform answered with is still a failed read", async () => {
    await screen("orders", 17);
    expect(readStatus()).toBe("ok");
    cleanup();

    // a 2xx refusal carrying a literal: the platform answered, so the dot may
    // not report the console as out of touch
    await screen("orders", 4);
    expect(readStatus()).toBe("ok");
    cleanup();

    // these two are answers as well, and neither is one the author can act on:
    // a read that was refused is a read that did not happen
    await screen("refused-authorization", 17);
    expect(readStatus()).toBe("fail");
    cleanup();

    await screen("refused-contract-version", 17);
    expect(readStatus()).toBe("fail");
    cleanup();

    await screen("network-down", 17);
    expect(readStatus()).toBe("fail");
  });

  it("lets an older read move nothing — not the dot, not the store, not the screen", async () => {
    const { container } = await screen("orders", 17);

    const answers: ((result: ReadResult<Draft>) => void)[] = [];
    vi.spyOn(fixtureReader.drafts, "get").mockImplementation(
      () =>
        new Promise<ReadResult<Draft>>((resolve) => {
          answers.push(resolve);
        }),
    );
    const refresh = screenContribution().actions.find((action) => action.id === "refresh");
    refresh?.run();
    flush();
    refresh?.run();
    flush();
    refresh?.run();
    flush();
    expect(answers).toHaveLength(3);

    // the newest read lands first, and it is the answer the author is waiting on
    answers[2](ok(unattributedDraft));
    await settle();
    expect(container.querySelector("h1")).toHaveTextContent("DRAFT charges @ 3");
    expect(visitedOfKind("draft")[0]).toMatchObject({ id: "charges" });

    // an older read that succeeded may not write the store: §2.0's tree would
    // then name the draft the author was answered past as the one they are on
    answers[0](ok(loopingDraft));
    await settle();
    expect(visitedOfKind("draft")[0]).toMatchObject({ id: "charges" });
    expect(visitedOfKind("draft").map((entry) => entry.id)).not.toContain("notifications");

    // and an older read that failed may not move the dot
    answers[1](
      err({
        kind: "network",
        message: "authoring request failed before receiving an HTTP response",
      }),
    );
    await settle();
    expect(readStatus()).toBe("ok");
    expect(container.querySelector("h1")).toHaveTextContent("DRAFT charges @ 3");
  });

  it("writes nothing once the author has left the screen", async () => {
    const answers: ((result: ReadResult<Draft>) => void)[] = [];
    vi.spyOn(fixtureReader.drafts, "get").mockImplementation(
      () =>
        new Promise<ReadResult<Draft>>((resolve) => {
          answers.push(resolve);
        }),
    );

    const view = render(() => <DraftScreen id="orders" revision={17} />);
    expect(answers).toHaveLength(1);
    view.unmount();

    // the read the author navigated away from still settles, and §1.4's dot
    // describes the screen they are on now — as does §2.0's tree
    answers[0](ok(parsingDraft));
    await settle();
    expect(readStatus()).toBe("never-contacted");
    expect(visitedOfKind("draft")).toEqual([]);
  });

  it("writes the draft to the visited store the moment the platform answers", async () => {
    await screen("orders", 17);

    // §2.0 addresses a draft by id *and* revision, and a draft has no verdict
    expect(visitedOfKind("draft")[0]).toMatchObject({
      id: "orders",
      revision: 17,
      label: "orders@17",
      verdict: "none",
    });
  });

  it("states a fault in a part rather than blanking the screen", async () => {
    const { container } = await screen(FAULT_DRAFT_ID, 1);

    expect(container.querySelector(".draft-fault")).toHaveTextContent(
      "the draft screen failed to render",
    );
    expect(container.querySelector(".draft-fault")).toHaveTextContent(
      "the stored bytes could not be read",
    );
    // the boundary is inside the screen, so the screen itself is still standing
    expect(container.querySelector(".draft-screen")).toBeInTheDocument();
  });

  it("re-issues the one read from the one refresh control", async () => {
    const { container, getByRole } = await screen("orders", 17);

    const refresh = getByRole("button", { name: /refresh this screen/ });
    expect(refresh).toHaveTextContent(/refreshed \d\d:\d\d:\d\d/);

    fireEvent.click(refresh);
    flush();
    // the read is in flight again, and the bytes already on the screen stay
    expect(refresh).toBeDisabled();
    expect(container).not.toHaveTextContent("loading the draft");

    await settle();
    expect(refresh).toBeEnabled();
    expect(shownSource(container)).toContain('"flow-id": "orders"');
  });
});

describe("DraftScreen — §2.4's parsing draft", () => {
  it("draws the wireframe's verdict and header line", async () => {
    const { container, getByRole } = await screen("orders", 17);

    // a draft has no verdict of its own (§2.0's `•`), so the bar states what
    // the screen is looking at and tones itself neutral
    expect(container.querySelector(".verdict-bar")).toHaveAttribute("data-tone", "neutral");
    expect(container.querySelector("h1")).toHaveTextContent("DRAFT orders @ 17");
    expect(head(container)).toContain("saved 2026-08-14 09:12 UTC");
    getByRole("button", { name: `copy draft id ${parsingDraft.draftId}` });
    // one screen, one heading: the verdict (wamn-dggp.21)
    expect(container.querySelectorAll("h1")).toHaveLength(1);
  });

  it("counts the draft's own bytes, with the thousands grouped", async () => {
    const { container } = await screen("orders", 17);
    const grouped = new Intl.NumberFormat("en-US").format(parsingDraft.byteLength);

    // the UTF-8 byte count the draft carries, never the line or character count
    expect(head(container)).toContain(`${grouped} bytes`);
    expect(grouped).toContain(",");
    expect(head(container)).not.toContain(`${parsingDraft.byteLength} bytes`);
  });

  it("shows the exact stored bytes, and shows them first", async () => {
    const { container, getByRole } = await screen("orders", 17);

    expect(panelLabel(container)).toBe("source");
    expect(container.querySelector(".draft-structure")).toBeNull();
    expect(getByRole("button", { name: "show this draft's source" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    // byte-faithful: the trailing newline terminates line 44 rather than
    // starting a line 45, which is the one difference the gutter is allowed
    expect(`${shownSource(container)}\n`).toBe(parsingDraft.definition);
    // nothing is tinted on a draft that parses
    expect(container.querySelector(".source-line[data-tone]")).toBeNull();
  });

  it("opens the structure tab onto the nodes and the grouped edges", async () => {
    const { container, getByRole } = await screen("orders", 17);

    const structure = getByRole("button", { name: "show this draft's structure" });
    expect(structure).toBeEnabled();
    fireEvent.click(structure);
    flush();

    expect(panelLabel(container)).toBe("structure");
    expect(structure).toHaveAttribute("aria-pressed", "true");
    // one tab stands at a time: the bytes are not still underneath it
    expect(container.querySelector(".source-view")).toBeNull();

    const ids = [...container.querySelectorAll("tbody .data-table-row")].map(
      (row) => row.querySelector(".data-table-cell")?.textContent ?? "",
    );
    expect(ids.map((id) => id.replace(/[▸▾]/g, ""))).toEqual([
      "ingress",
      "parse-order",
      "check-stock",
      "fetch-inventory",
      "reserve",
      "respond",
    ]);
    expect(
      [...container.querySelectorAll(".draft-edge-source")].map((from) => from.textContent),
    ).toEqual([
      "from ingress",
      "from parse-order",
      "from check-stock",
      "from fetch-inventory",
      "from reserve",
    ]);
    expect(container.querySelectorAll(".draft-edge")).toHaveLength(6);
  });

  it("offers the palette the panel that is standing, and the tabs as actions", async () => {
    const { container } = await screen("orders", 17);

    // the anchor is the panel the reader would land on; the other tab is not on
    // the page, so it is offered as the action that puts it there
    expect(screenContribution().anchors).toEqual([{ id: "draft-source", label: "source" }]);
    expect(screenContribution().actions.map((action) => action.id)).toEqual([
      "show-source",
      "show-structure",
      "refresh",
    ]);

    const show = screenContribution().actions.find((action) => action.id === "show-structure");
    show?.run();
    flush();

    expect(panelLabel(container)).toBe("structure");
    expect(screenContribution().anchors).toEqual([{ id: "draft-structure", label: "structure" }]);
    // the anchor names an element that is really there, and can be landed on
    expect(document.getElementById("draft-structure")).toHaveAttribute("tabindex", "-1");
  });
});

describe("DraftScreen — a revision that stops parsing under the reader", () => {
  it("falls back to the bytes rather than standing a structure panel over nothing", async () => {
    const { container, getByRole } = await screen("orders", 17);

    fireEvent.click(getByRole("button", { name: "show this draft's structure" }));
    flush();
    expect(panelLabel(container)).toBe("structure");

    // the same id and revision, re-read into bytes that no longer parse
    const stopped: Draft = {
      ...parsingDraft,
      definition: nonParsingDraft.definition,
      byteLength: nonParsingDraft.byteLength,
      parse: nonParsingDraft.parse,
    };
    vi.spyOn(fixtureReader.drafts, "get").mockResolvedValue(ok(stopped));
    screenContribution().actions.find((action) => action.id === "refresh")?.run();
    await settle();

    // what the reader asked for is not what can stand: a structure panel over a
    // document with no structure is a label promising a flow over an empty lane
    expect(panelLabel(container)).toBe("source");
    expect(container.querySelector(".draft-structure")).toBeNull();
    expect(container.querySelector(".source-view")).toBeInTheDocument();
    expect(getByRole("button", { name: "show this draft's structure" })).toBeDisabled();
    // and the palette stops offering a tab that could not stand if it were taken
    expect(screenContribution().actions.map((action) => action.id)).toEqual([
      "show-source",
      "refresh",
    ]);
    expect(screenContribution().anchors).toEqual([{ id: "draft-source", label: "source" }]);
  });
});

describe("DraftScreen — the draft that does not parse", () => {
  it("disables the structure tab and says why beside it", async () => {
    const { container, getByRole } = await screen("orders", 16);

    const structure = getByRole("button", { name: "show this draft's structure" });
    expect(structure).toBeDisabled();
    // a disabled control cannot be focused, so its state is stated beside it
    expect(container.querySelector(".draft-tab-refusal")).toHaveTextContent(
      "does not parse — line 41: Expected ':' after property name",
    );
    // and the bytes still stand: a draft that does not parse is a normal object
    expect(panelLabel(container)).toBe("source");
    expect(container.querySelector(".draft-structure")).toBeNull();
  });

  it("tints the line the parse failed on, and only that line", async () => {
    const { container } = await screen("orders", 16);

    const tinted = [...container.querySelectorAll(".source-line[data-tone]")];
    expect(tinted).toHaveLength(1);
    expect(tinted[0].querySelector(".source-number")).toHaveTextContent("41");
    // the line the fixture broke: `"schema-version"` lost its colon
    expect(tinted[0].querySelector(".source-text")).toHaveTextContent('"schema-version" "0.1",');
    // the tint is never the only carrier
    expect(tinted[0]).toHaveTextContent("parse error on line 41");
  });

  it("offers the palette no way into a structure that is not there", async () => {
    await screen("orders", 16);

    expect(screenContribution().actions.map((action) => action.id)).toEqual([
      "show-source",
      "refresh",
    ]);
    expect(screenContribution().anchors).toEqual([{ id: "draft-source", label: "source" }]);
  });

  it("keeps the provenance of a dirty working tree from being read as the commit", async () => {
    const { container } = await screen("orders", 16);

    expect(container.querySelector(".status-badge")).toHaveTextContent("dirty");
    expect(container.querySelector(".status-badge")).toHaveAttribute("data-tone", "warn");
    // the platform never ran git, and the block says so rather than implying it
    expect(container).toHaveTextContent("the client's own claim at save time");
    expect(container).toHaveTextContent("descended from this commit, not equal to it");
    // this fixture's provenance carries no ref, and the pair says so
    expect(pair(container, "ref")).toBe("no ref recorded");
    expect(pair(container, "commit")).toContain(nonParsingDraft.provenance?.commit.slice(0, 4));
  });
});

describe("DraftScreen — the draft nothing was recorded about", () => {
  it("states both absences in words, and prints no empty value anywhere", async () => {
    const { container } = await screen("charges", 3);

    // §2.4's header line, whole: a missing save time is said, never left blank,
    // and no separator on it stands beside a value that was never printed
    expect(head(container)).toBe(
      `this read carries no save time · ${unattributedDraft.byteLength} bytes · draft charges⧉ · [source|structure]`,
    );
    expect(head(container)).not.toContain("saved ");

    // both absences are this read's, not the draft's: neither `savedAt` nor
    // provenance has a read-side source (types.ts STEP-9 RISK), so a sentence
    // about what was recorded with the draft would be a claim about records
    // this console has never seen
    expect(container.querySelector(".empty-state")).toHaveTextContent(
      "this read carries no provenance",
    );
    expect(container).not.toHaveTextContent("recorded with this draft");
    // nothing claims a working tree the read never carried
    expect(container.querySelector(".status-badge")).toBeNull();
    expect(container.querySelector(".key-value")).toBeNull();

    // whatever the screen did print, it printed words
    const printed = [
      ...container.querySelectorAll(".draft-absent, .empty-state, .key-value-value, .status-badge"),
    ];
    expect(printed.filter((element) => !/\p{L}/u.test(element.textContent ?? ""))).toEqual([]);
  });

  it("says a clean working tree is a claim too, once there is one to state", async () => {
    const { container } = await screen("notifications", 2);

    expect(container.querySelector(".status-badge")).toHaveTextContent("clean");
    expect(container).toHaveTextContent("the client's own claim at save time");
    // only a dirty tree needs the sentence about descent
    expect(container).not.toHaveTextContent("descended from this commit");
  });
});

describe("DraftScreen — the flow that loops", () => {
  it("draws the cyclic edge as an ordinary edge of its source's group", async () => {
    const { container, getByRole } = await screen("notifications", 2);

    fireEvent.click(getByRole("button", { name: "show this draft's structure" }));
    flush();

    const groups = [...container.querySelectorAll(".draft-edge-group")];
    expect(groups.map((group) => group.querySelector(".draft-edge-source")?.textContent)).toEqual([
      "from ingress",
      "from notify-subscriber",
      "from next-subscriber",
    ]);
    // the edge back to an earlier node is one row of one group, marked as
    // nothing else: whether the loop terminates is not this screen's to say
    const cycle = groups[2].querySelectorAll(".draft-edge");
    expect(cycle).toHaveLength(1);
    expect(cycle[0]).toHaveTextContent("next-subscriber");
    expect(cycle[0]).toHaveTextContent("notify-subscriber");
    expect(container.querySelectorAll(".draft-edge")).toHaveLength(
      loopingDraft.definition.split(`{ "from"`).length - 1,
    );
  });
});

describe("DraftScreen — §3's floor, on both tabs at once", () => {
  it("names every control for what it acts on, in both tabs, twice over", async () => {
    const { container, getByRole } = await screen("orders", 17);

    const check = (where: string): string[] => {
      const offences: string[] = [];
      const names = [...container.querySelectorAll("button")].map(reachableText);
      for (const name of names.filter((name) => !/\p{L}/u.test(name))) {
        offences.push(`${where}: a button is named ${JSON.stringify(name)}`);
      }
      // two controls of one screen act on two different things, so a name that
      // repeats is a name that stopped saying which
      if (new Set(names).size !== names.length) {
        offences.push(`${where}: two buttons share a name — ${names.join(", ")}`);
      }
      for (const toned of container.querySelectorAll("[data-tone]")) {
        if (!/\p{L}/u.test(reachableText(toned))) {
          offences.push(`${where}: [data-tone] says nothing a reader reaches`);
        }
      }
      for (const table of container.querySelectorAll("table")) {
        if (table.querySelector(":scope > caption") === null) {
          offences.push(`${where}: a table carries no caption`);
        }
      }
      return offences;
    };

    expect(check("source")).toEqual([]);

    fireEvent.click(getByRole("button", { name: "show this draft's structure" }));
    flush();
    expect(check("structure")).toEqual([]);

    // and with every node's config open, which is the state that puts the most
    // controls on this screen at once
    for (const toggle of container.querySelectorAll<HTMLElement>(
      'button[aria-expanded="false"]',
    )) {
      fireEvent.click(toggle);
    }
    flush();
    expect(check("structure, opened")).toEqual([]);
    expect(container.querySelectorAll(".data-table-disclosed").length).toBeGreaterThan(0);
  });
});

describe("DraftScreen — a document that parses and is not a flow", () => {
  it("opens the structure tab onto stated absences rather than failing", async () => {
    const { container, getByRole } = await screen(HALF_FINISHED_ID, 1);

    // it parses, so the tab stands: `parse.ok` is a fact about the bytes, not a
    // promise about the document's shape
    const structure = getByRole("button", { name: "show this draft's structure" });
    expect(structure).toBeEnabled();
    expect(container.querySelector(".draft-tab-refusal")).toBeNull();

    fireEvent.click(structure);
    flush();

    expect(container.querySelector(".draft-fault")).toBeNull();
    expect(container).toHaveTextContent(`this document carries no "nodes" field`);
    expect(container).toHaveTextContent(`this document carries no "edges" field`);
    expect(container.querySelectorAll("tbody .data-table-row")).toHaveLength(0);
    expect(container.querySelectorAll(".draft-edge")).toHaveLength(0);
    // and the bytes are still one click away, which is what the author came for
    fireEvent.click(getByRole("button", { name: "show this draft's source" }));
    flush();
    expect(`${shownSource(container)}\n`).toBe(halfFinishedSource);
  });
});
