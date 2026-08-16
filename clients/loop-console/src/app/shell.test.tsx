import { cleanup, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { proxyTarget } from "../config";
import { FAILING_RUN_ID, FINALIZED_REPORT_ID } from "../reader/fixtures";
import type { Route } from "../routing/route";
import { clearVisited } from "../store/visited";
import { AppShell } from "./app-shell";
import { setReadStatus } from "./read-status";

afterEach(() => {
  cleanup();
  setReadStatus("never-contacted");
  // the run route records its read in the visited store, which outlives a test
  clearVisited();
  // so does the address bar, and a palette navigation writes it
  window.location.hash = "";
});

function shell(route: Route) {
  return render(() => <AppShell route={route} />);
}

/** The run route reads through the reader seam; two ticks settle its read. */
async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  flush();
}

describe("AppShell", () => {
  it("renders each route's screen inside the shell", async () => {
    const cases: Array<{ route: Route; expected: string[] }> = [
      // the start route mounts §2.1's screen, which states where the console is
      // pointed and offers the one field that gets an author to a verdict
      {
        route: { kind: "start" },
        expected: ["wamn loop console", "dev target:", "paste a run, report or draft id"],
      },
      // the run route mounts the run screen, which answers out of the reader
      {
        route: { kind: "run", id: FAILING_RUN_ID },
        expected: ["FAILED · retry-exhausted at fetch-inventory", "execution", "details"],
      },
      // and the report and draft routes mount theirs, which do the same
      {
        route: { kind: "report", id: FINALIZED_REPORT_ID },
        expected: ["11 / 12 PASSED", "cases", "backorders-when-out-of-stock"],
      },
      {
        route: { kind: "draft", id: "orders", revision: "17" },
        expected: ["DRAFT orders @ 17", "source"],
      },
      { route: { kind: "not-found", hash: "#/logs" }, expected: ["not found", "hash #/logs"] },
    ];

    for (const { route, expected } of cases) {
      const { container } = shell(route);
      await settle();
      // chrome above the column, and the panel region beside it
      expect(container.querySelector("header")).toHaveTextContent("wamn loop");
      expect(container.querySelector("aside")).toBeInTheDocument();

      const column = container.querySelector("main");
      expect(column).toBeInTheDocument();
      for (const text of expected) {
        expect(column).toHaveTextContent(text);
      }
      cleanup();
    }
  });

  it("tells a revision the router cannot read from one the reader cannot find", async () => {
    /*
     * Two different absences that a single "not found" would blur. A segment
     * that names no revision names no draft either, so the router answers and
     * no read is ever issued. A segment that *does* name a revision addresses a
     * draft that may simply not exist — which is the platform's answer, reached
     * through the reader, and the screen shows it as a read that was refused.
     */
    const opaque = shell({ kind: "draft", id: "orders", revision: "head rev" }).container;
    await settle();
    expect(opaque.querySelector("main .screen-name")).toHaveTextContent("not found");
    expect(opaque.querySelector("main")).toHaveTextContent("hash #/draft/orders/head%20rev");
    cleanup();

    const numeric = shell({ kind: "draft", id: "orders", revision: "17" }).container;
    await settle();
    // the draft screen mounted: its verdict is the heading, and there is no
    // router placeholder above it
    expect(numeric.querySelector("main .screen-name")).toBeNull();
    expect(numeric.querySelector("main h1")).toHaveTextContent("DRAFT orders @ 17");
    cleanup();

    // Revision 0 converts — it is the one revision a truthiness check loses —
    // so this reaches the reader, which refuses it. The router must not.
    const zero = shell({ kind: "draft", id: "orders", revision: "0" }).container;
    await settle();
    expect(zero.querySelector("main .screen-name")).toBeNull();
    expect(zero.querySelector("main")).toHaveTextContent("draft-revision-not-found");
  });

  it("gives the run route one heading, and it is the verdict", async () => {
    const { container } = shell({ kind: "run", id: FAILING_RUN_ID });
    await settle();

    const headings = container.querySelectorAll("h1");
    expect(headings).toHaveLength(1);
    expect(headings[0]).toHaveTextContent("FAILED · retry-exhausted at fetch-inventory");
    // the step-1 placeholder heading is gone from this route (wamn-dggp.21)
    expect(container.querySelector(".screen-name")).toBeNull();
  });

  it("renders the top bar's wordmark, proxy target, status word, and jump hint", () => {
    const { container } = shell({ kind: "start" });
    const bar = container.querySelector("header");
    expect(bar).toHaveTextContent("wamn loop");
    expect(bar).toHaveTextContent(proxyTarget);
    expect(bar).toHaveTextContent("never contacted");
    expect(bar).toHaveTextContent("⌘K");
  });

  /** ⌘K, sent at the document the way a reader's keyboard reaches the console. */
  function commandKey(): KeyboardEvent {
    const event = new KeyboardEvent("keydown", {
      key: "k",
      metaKey: true,
      bubbles: true,
      cancelable: true,
    });
    document.dispatchEvent(event);
    flush();
    return event;
  }

  it("mounts the command palette, which draws nothing until ⌘K asks for it", () => {
    const { container } = shell({ kind: "start" });
    expect(container.querySelector("[role='dialog']")).toBeNull();

    expect(commandKey().defaultPrevented).toBe(true);
    const palette = container.querySelector("[role='dialog']");
    expect(palette).toHaveAttribute("aria-label", "command palette");
    // §6 step 7: no keybinding column anywhere — the palette teaches no keys
    // because there are none outside it to teach
    expect(palette).not.toHaveTextContent("⌘");
    expect(palette).not.toHaveTextContent("Ctrl");
  });

  it("offers the mounted screen's own sections and actions, and the run it just read", async () => {
    const { container } = shell({ kind: "run", id: FAILING_RUN_ID });
    await settle();
    commandKey();

    const palette = container.querySelector("[role='dialog']");
    // §6 step 7's done-check: every navigation and view action reachable here
    expect(palette).toHaveTextContent("go to execution");
    expect(palette).toHaveTextContent("expand all execution rows");
    expect(palette).toHaveTextContent("refresh this screen");
    expect(palette).toHaveTextContent(`open run ${FAILING_RUN_ID}`);
  });

  it("leaves a ⌘K navigation's reader in the column, and not on <body>", async () => {
    const { container } = shell({ kind: "run", id: FAILING_RUN_ID });
    await settle();

    // stand where a reader actually stands: on a control of the screen the
    // navigation is about to unmount
    const control = container.querySelector<HTMLButtonElement>("main button");
    control?.focus();
    expect(document.activeElement).toBe(control);

    commandKey();
    const start = [...container.querySelectorAll<HTMLButtonElement>("[role='dialog'] button")].find(
      (row) => (row.textContent ?? "").includes("go to start"),
    );
    start?.click();
    flush();
    expect(window.location.hash).toBe("#/");

    // §6 step 7 promises focus restore on close, and this shell has nothing
    // focusable outside the column — the top bar is spans and the panel is
    // empty — so restoring to the unmounted control would put a keyboard reader
    // on <body>, a full Tab from the top of the screen they just asked for
    expect(document.activeElement).toBe(container.querySelector("main"));
    expect(document.activeElement).not.toBe(document.body);
  });

  it("states read status in words, and updates them reactively", () => {
    // the word has to reach assistive tech, so read the visually-hidden node —
    // the dot's title attribute sits on aria-hidden markup and does not count
    const { container } = shell({ kind: "start" });
    const label = () => container.querySelector(".visually-hidden");
    const dot = () => container.querySelector(".status-dot");

    expect(label()).toHaveTextContent("never contacted");
    expect(dot()).toHaveAttribute("data-status", "never-contacted");

    setReadStatus("fail");
    flush();
    expect(label()).toHaveTextContent("last read failed");
    expect(dot()).toHaveAttribute("data-status", "fail");

    setReadStatus("ok");
    flush();
    expect(label()).toHaveTextContent("last read succeeded");
    expect(dot()).toHaveAttribute("data-status", "ok");
  });
});
