import { cleanup, render } from "@solidjs/testing-library";
import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { effectUncertainRun, terminalizedRun } from "../reader/fixtures";
import { EFFECT_UNCERTAIN_MEANING, type EffectUncertainty } from "../reader/types";
import { UncertainPanel } from "./uncertain-panel";

afterEach(cleanup);

/** Both runs carry one; the panel's whole subject is an uncertainty that exists. */
function uncertaintyOf(uncertainty: EffectUncertainty | null): EffectUncertainty {
  if (uncertainty === null) {
    throw new Error("fixture carries no effect uncertainty");
  }
  return uncertainty;
}

const unresolved = uncertaintyOf(effectUncertainRun.uncertainty);
const terminalized = uncertaintyOf(terminalizedRun.uncertainty);

function sentences(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".uncertain-sentence")].map(
    (sentence) => sentence.textContent ?? "",
  );
}

function record(container: HTMLElement): Array<[string, string]> {
  return [...container.querySelectorAll(".key-value")].map((pair) => [
    pair.querySelector(".key-value-label")?.textContent ?? "",
    pair.querySelector(".key-value-value")?.textContent ?? "",
  ]);
}

/** The resolution's own divider, not whichever label the panel prints first. */
function resolutionLabel(container: HTMLElement): Element | null {
  return container.querySelector(".uncertain-resolution .section-label");
}

describe("UncertainPanel", () => {
  it("prints the platform's meaning verbatim, every sentence, unaltered", () => {
    // pinned against the constant itself: a paraphrase, a dropped sentence, or a
    // console sentence added among them all fail here, which is the point of it
    const { container } = render(() => <UncertainPanel uncertainty={unresolved} />);

    expect(sentences(container)).toEqual([...EFFECT_UNCERTAIN_MEANING]);
  });

  it("keeps §1.1's one purple with the verbatim status word", () => {
    const { container } = render(() => <UncertainPanel uncertainty={unresolved} />);

    const panel = container.querySelector('[data-tone="uncertain"]');
    expect(panel).toHaveClass("uncertain-panel");
    const badge = container.querySelector(".status-badge");
    expect(badge).toHaveTextContent("effect-uncertain");
    expect(badge).toHaveAttribute("data-tone", "uncertain");
  });

  it("opens its sections §1.4's way, and never raises a second verdict rule", () => {
    // §1.3 gives the 3px status rule to the verdict bar alone: every heading the
    // panel owns is a section label, and it owns no head-sized rule of its own
    const { container } = render(() => <UncertainPanel uncertainty={unresolved} />);

    const headings = [...container.querySelectorAll("h1, h2, h3, h4, h5, h6")];
    expect(headings.map((heading) => heading.textContent)).toEqual([
      "effect uncertain",
      "operator resolution",
    ]);
    expect(headings.every((heading) => heading.classList.contains("section-label-text"))).toBe(true);
  });

  it("says plainly that nothing is recorded while the run is unresolved", () => {
    const { container } = render(() => <UncertainPanel uncertainty={unresolved} />);

    expect(resolutionLabel(container)).toHaveTextContent("unresolved");
    // the read carries no operator action; whether one happened is not ours to say
    expect(container.querySelector(".empty-state")).toHaveTextContent(
      "no operator action is recorded for this run",
    );
    // no decision was recorded, so there is no record — and no blank pairs standing in for one
    expect(record(container)).toEqual([]);
    expect(container).not.toHaveTextContent("terminalized");
  });

  it("renders the whole action a terminalized run recorded", () => {
    const { container } = render(() => <UncertainPanel uncertainty={terminalized} />);

    expect(resolutionLabel(container)).toHaveTextContent("operator-terminalized");
    expect(record(container)).toEqual([
      ["basis", "counterparty-confirmation"],
      ["evidence", "ops/2026-08-14/psp-settlement-report.csv"],
      ["operator", "wamn_project_admin"],
      ["reason", "operator-terminalized-effect-uncertain"],
      ["when", "2026-08-14T11:41:02Z"],
    ]);
    // the two resolution states are exclusive: a record under `nothing is
    // recorded` is the one self-contradiction this panel must never print
    expect(container.querySelector(".empty-state")).toBeNull();
  });

  it("still reads effect-uncertain over that record, as the run itself does", () => {
    // the resolved triple keeps the run's failure kind `effect-uncertain`; a
    // panel that closed the state over the record would contradict the read
    const { container } = render(() => <UncertainPanel uncertainty={terminalized} />);

    expect(container.querySelector(".status-badge")?.textContent).toBe("effect-uncertain");
    expect(sentences(container)).toEqual([...EFFECT_UNCERTAIN_MEANING]);
  });

  it("states an absent evidence ref, which no fixture carries", () => {
    // a shape, not domain data: `operator-judgment` rests on no reference at all,
    // and the type admits the null the fixtures never take
    const judged: EffectUncertainty = {
      meaning: EFFECT_UNCERTAIN_MEANING,
      resolution: {
        state: "operator-terminalized",
        basis: "operator-judgment",
        evidenceRef: null,
        principal: "wamn_project_admin",
        terminalReason: "operator-terminalized-effect-uncertain",
        at: "2026-08-14T11:41:02Z",
      },
    };
    const { container } = render(() => <UncertainPanel uncertainty={judged} />);

    expect(record(container)[1]).toEqual(["evidence", "none recorded"]);
    expect(container.querySelector(".uncertain-absent")).toHaveClass("frame");
    // the rest of the record is untouched by the one field it lacks
    expect(record(container)[0]).toEqual(["basis", "operator-judgment"]);
    expect(record(container)).toHaveLength(5);
  });

  it("prints the basis and the terminal reason as they arrive, known or not", () => {
    // `terminal_reason` has no CHECK constraint and one known member, so an
    // unheard-of reason must read back as itself rather than blanked or mapped
    const unheardOf: EffectUncertainty = {
      meaning: EFFECT_UNCERTAIN_MEANING,
      resolution: {
        state: "operator-terminalized",
        basis: "external-evidence",
        evidenceRef: "ops/2026-08-14/psp-settlement-report.csv",
        principal: "wamn_project_admin",
        terminalReason: "operator-abandoned-effect-uncertain",
        at: "2026-08-14T11:41:02Z",
      },
    };
    const { container } = render(() => <UncertainPanel uncertainty={unheardOf} />);

    expect(record(container)[0]).toEqual(["basis", "external-evidence"]);
    expect(record(container)[3]).toEqual(["reason", "operator-abandoned-effect-uncertain"]);
  });

  it("states a read that carried no meaning rather than standing empty", () => {
    // `meaning` is `readonly string[]`, which admits the empty array; a dominant
    // section whose whole subject is missing must say so
    const wordless: EffectUncertainty = { meaning: [], resolution: { state: "unresolved" } };
    const { container } = render(() => <UncertainPanel uncertainty={wordless} />);

    expect(sentences(container)).toEqual([]);
    expect(container.querySelector(".uncertain-meaning .empty-state")).toHaveTextContent(
      "the read carried no meaning",
    );
  });

  it("moves the resolution when a refresh re-reads the run into one", () => {
    // §2.6's refresh re-issues the screen's one read. A panel frozen on the
    // first read would go on saying nothing is recorded after something is.
    const [uncertainty, setUncertainty] = createSignal(unresolved);
    const { container } = render(() => <UncertainPanel uncertainty={uncertainty()} />);
    expect(container.querySelector(".empty-state")).toHaveTextContent(
      "no operator action is recorded for this run",
    );

    setUncertainty(terminalized);
    flush();

    expect(container.querySelector(".empty-state")).toBeNull();
    expect(resolutionLabel(container)).toHaveTextContent("operator-terminalized");
    expect(record(container)).toHaveLength(5);
  });
});
