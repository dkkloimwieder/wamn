import { renderToString } from "@solidjs/web";
import { describe, expect, it } from "vitest";

import { proxyTarget } from "../config";
import type { Route } from "../routing/route";
import { AppShell } from "./app-shell";
import { setReadStatus } from "./read-status";

/** The markup between the shell's content column tags. */
function column(markup: string): string {
  const open = markup.indexOf("<main");
  const close = markup.indexOf("</main>");
  expect(open).toBeGreaterThan(-1);
  expect(close).toBeGreaterThan(open);
  return markup.slice(open, close);
}

/** The text inside the shell's visually-hidden node, SSR markers stripped. */
function readStatusLabel(markup: string): string {
  const tag = 'class="visually-hidden">';
  const open = markup.indexOf(tag);
  expect(open).toBeGreaterThan(-1);
  const close = markup.indexOf("</span>", open);
  expect(close).toBeGreaterThan(open);
  return markup.slice(open + tag.length, close).replace(/<!--.*?-->/g, "");
}

function shell(route: Route): string {
  return renderToString(() => <AppShell route={route} />);
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
      const markup = shell(route);
      // chrome above the column, and the panel region beside it
      expect(markup).toContain("wamn loop");
      expect(markup).toContain("<aside");
      const inside = column(markup);
      for (const text of expected) {
        expect(inside).toContain(text);
      }
    }
  });

  it("renders the top bar's wordmark, proxy target, status word, and jump hint", () => {
    const markup = shell({ kind: "start" });
    expect(markup).toContain("wamn loop");
    expect(markup).toContain(proxyTarget);
    expect(markup).toContain("never contacted");
    expect(markup).toContain("⌘K");
  });

  it("states read status in words, not only in the dot's color", () => {
    // the word has to reach assistive tech, so pin it to the visually-hidden
    // node — the dot's title attribute sits on aria-hidden markup
    setReadStatus("fail");
    const failed = shell({ kind: "start" });
    expect(readStatusLabel(failed)).toContain("last read failed");
    expect(failed).toContain('data-status="fail"');

    setReadStatus("ok");
    const succeeded = shell({ kind: "start" });
    expect(readStatusLabel(succeeded)).toContain("last read succeeded");
    expect(succeeded).toContain('data-status="ok"');

    setReadStatus("never-contacted");
  });
});
