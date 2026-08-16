import { cleanup, render } from "@solidjs/testing-library";
import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import type { JSX } from "@solidjs/web";

import { contributeScreen, focusAnchor, screenContribution } from "./screen-actions";

afterEach(cleanup);

/** A screen whose offer changes after it mounts, the way a real read does. */
function Screen(props: { parses: boolean; onExpand: () => void }): JSX.Element {
  contributeScreen(() => ({
    anchors: props.parses
      ? [
          { id: "draft-source", label: "source" },
          { id: "draft-structure", label: "structure" },
        ]
      : [{ id: "draft-source", label: "source" }],
    actions: [{ id: "expand-all", label: "expand all", run: props.onExpand }],
  }));
  return (
    <>
      <div id="draft-source" tabindex="-1">
        source
      </div>
      {props.parses ? (
        <div id="draft-structure" tabindex="-1">
          structure
        </div>
      ) : null}
    </>
  );
}

describe("what a screen offers the palette", () => {
  it("offers nothing until a screen contributes", () => {
    expect(screenContribution()).toEqual({ anchors: [], actions: [] });
  });

  it("offers the screen's live answer, not the one it mounted with", () => {
    const [parses, setParses] = createSignal(false);
    render(() => <Screen parses={parses()} onExpand={() => {}} />);
    flush();

    // a draft that does not parse has no structure to offer
    expect(screenContribution().anchors.map((anchor) => anchor.label)).toEqual(["source"]);

    setParses(true);
    flush();
    expect(screenContribution().anchors.map((anchor) => anchor.label)).toEqual([
      "source",
      "structure",
    ]);
  });

  it("runs the screen's own callback, so the palette cannot drift into a second one", () => {
    let expanded = 0;
    render(() => <Screen parses onExpand={() => (expanded += 1)} />);
    flush();

    const action = screenContribution().actions[0];
    expect(action.label).toBe("expand all");
    action.run();
    expect(expanded).toBe(1);
  });

  it("releases the offer on unmount, so no anchor outlives its section", () => {
    const view = render(() => <Screen parses onExpand={() => {}} />);
    flush();
    expect(screenContribution().anchors).toHaveLength(2);

    view.unmount();
    flush();
    // otherwise the palette would offer to scroll to a section that is gone
    expect(screenContribution()).toEqual({ anchors: [], actions: [] });
  });

  it("replaces the previous screen's offer when the next one mounts", () => {
    const first = render(() => <Screen parses={false} onExpand={() => {}} />);
    flush();
    first.unmount();
    flush();

    render(() => <Screen parses onExpand={() => {}} />);
    flush();
    expect(screenContribution().anchors).toHaveLength(2);
  });

  it("keeps the mounted screen's offer when the outgoing one disposes late", () => {
    // A route change can mount the next screen before the previous one is
    // disposed. The late cleanup must not erase the offer of the screen the
    // reader is actually looking at.
    const outgoing = render(() => <Screen parses={false} onExpand={() => {}} />);
    flush();
    render(() => <Screen parses onExpand={() => {}} />);
    flush();
    expect(screenContribution().anchors).toHaveLength(2);

    outgoing.unmount();
    flush();
    expect(screenContribution().anchors.map((anchor) => anchor.label)).toEqual([
      "source",
      "structure",
    ]);
  });
});

describe("moving the reader to an anchor", () => {
  it("focuses the section so a keyboard reader arrives with it", () => {
    render(() => <Screen parses onExpand={() => {}} />);
    flush();

    expect(focusAnchor("draft-structure")).toBe(true);
    expect(document.activeElement).toBe(document.getElementById("draft-structure"));
  });

  it("answers false for a section that is not on the page", () => {
    render(() => <Screen parses={false} onExpand={() => {}} />);
    flush();

    // the structure tab is not rendered, so there is nothing to move to and
    // the caller has to say so rather than silently doing nothing
    expect(focusAnchor("draft-structure")).toBe(false);
  });
});
