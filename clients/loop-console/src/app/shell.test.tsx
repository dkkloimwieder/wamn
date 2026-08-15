import { cleanup, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { proxyTarget } from "../config";
import type { Route } from "../routing/route";
import { AppShell } from "./app-shell";
import { setReadStatus } from "./read-status";

afterEach(() => {
  cleanup();
  setReadStatus("never-contacted");
});

function shell(route: Route) {
  return render(() => <AppShell route={route} />);
}

describe("AppShell", () => {
  it("renders each route's placeholder inside the shell", () => {
    const cases: Array<{ route: Route; expected: string[] }> = [
      { route: { kind: "start" }, expected: ["start"] },
      { route: { kind: "run", id: "01J9X3F2K" }, expected: ["run", "id 01J9X3F2K"] },
      { route: { kind: "report", id: "01J9X8Q11" }, expected: ["report", "id 01J9X8Q11"] },
      {
        route: { kind: "draft", id: "orders", revision: "17" },
        expected: ["draft", "id orders", "revision 17"],
      },
      { route: { kind: "not-found", hash: "#/logs" }, expected: ["not found", "hash #/logs"] },
    ];

    for (const { route, expected } of cases) {
      const { container } = shell(route);
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

  it("renders not found for a draft revision that is not a revision", () => {
    // the heading is what tells one screen from another; the not-found body
    // echoes the hash, so it contains "draft" too
    const opaque = shell({ kind: "draft", id: "orders", revision: "head rev" }).container;
    expect(opaque.querySelector("main .screen-name")).toHaveTextContent("not found");
    expect(opaque.querySelector("main")).toHaveTextContent("hash #/draft/orders/head%20rev");
    cleanup();

    const numeric = shell({ kind: "draft", id: "orders", revision: "17" }).container;
    expect(numeric.querySelector("main .screen-name")).toHaveTextContent("draft");
    expect(numeric.querySelector("main")).toHaveTextContent("revision 17");
    cleanup();

    // revision 0 converts, and it is the one revision a truthiness check loses
    const zero = shell({ kind: "draft", id: "orders", revision: "0" }).container;
    expect(zero.querySelector("main .screen-name")).toHaveTextContent("draft");
    expect(zero.querySelector("main")).toHaveTextContent("revision 0");
    expect(zero.querySelector("main")).not.toHaveTextContent("not found");
  });

  it("renders the top bar's wordmark, proxy target, status word, and jump hint", () => {
    const { container } = shell({ kind: "start" });
    const bar = container.querySelector("header");
    expect(bar).toHaveTextContent("wamn loop");
    expect(bar).toHaveTextContent(proxyTarget);
    expect(bar).toHaveTextContent("never contacted");
    expect(bar).toHaveTextContent("⌘K");
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
