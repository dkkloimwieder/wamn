import { cleanup, render } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { ReadResult, ReaderError } from "../reader/errors";
import { fixtureReader } from "../reader/fixture-reader";
import { ErrorPanel } from "./error-panel";

afterEach(cleanup);

/** Every state below comes from a read the fixture reader actually refused. */
function refused<T>(result: ReadResult<T>): ReaderError {
  if (result.ok) {
    throw new Error("expected the read to be refused");
  }
  return result.error;
}

function panel(error: ReaderError): HTMLElement {
  const { container } = render(() => <ErrorPanel error={error} />);
  return container.querySelector(".error-panel") as HTMLElement;
}

/** What the platform said, read back without normalisation of any kind. */
function said(root: HTMLElement): string | null {
  return root.querySelector(".error-panel-said")?.textContent ?? null;
}

describe("ErrorPanel", () => {
  it("renders each resource's not-found literal, and what was asked for", async () => {
    const run = panel(refused(await fixtureReader.runs.get("01J9NOTHINGSTOREDHEREXX")));
    expect(said(run)).toBe("run-not-found");
    expect(run).toHaveTextContent("not found");
    expect(run).toHaveTextContent("resource");
    expect(run).toHaveTextContent("run");
    expect(run).toHaveTextContent("01J9NOTHINGSTOREDHEREXX");
    // a missing resource is an answer, not a transport failure
    expect(run).toHaveTextContent("the platform answered, and holds no run under this id.");
    expect(run).toHaveAttribute("data-tone", "neutral");
    cleanup();

    const report = panel(refused(await fixtureReader.reports.get("01J9NOSUCHREPORTXXXXXXX")));
    expect(said(report)).toBe("report-not-found");
    expect(report).toHaveTextContent("01J9NOSUCHREPORTXXXXXXX");
    cleanup();

    const draft = panel(refused(await fixtureReader.drafts.get("orders", 99)));
    expect(said(draft)).toBe("draft-revision-not-found");
    // only a draft not-found carries one, and it is what the route asked for
    expect(draft).toHaveTextContent("revision");
    expect(draft).toHaveTextContent("99");
  });

  it("names no revision on a run or report not-found", async () => {
    const run = panel(refused(await fixtureReader.runs.get("01J9NOTHINGSTOREDHEREXX")));
    expect(run).not.toHaveTextContent("revision");
  });

  it("renders the authorization refusal verbatim, and separates it from a not-found", async () => {
    const refusal = panel(refused(await fixtureReader.reports.get("refused-authorization")));

    expect(said(refusal)).toBe("authorization-denied");
    expect(refusal).toHaveTextContent("refused");
    expect(refusal).toHaveTextContent("the credentials presented are not authorized for it");
    expect(refusal).toHaveAttribute("data-tone", "warn");
    // the refusal value carries no resource or id, so the panel claims neither
    expect(refusal).not.toHaveTextContent("resource");
    expect(refusal).not.toHaveTextContent("detail");
  });

  it("renders the contract-version refusal with whatever it carried alongside", async () => {
    const refusal = panel(
      refused(await fixtureReader.drafts.get("refused-contract-version", 1)),
    );

    expect(said(refusal)).toBe("unsupported-contract-version");
    expect(refusal).toHaveTextContent("detail");
    expect(refusal).toHaveTextContent("requested 0.2, supported 0.1");
  });

  it("renders the transport failure as its own state, not as a refusal", async () => {
    const network = panel(refused(await fixtureReader.runs.get("network-down")));

    expect(said(network)).toBe("authoring request failed before receiving an HTTP response");
    expect(network).toHaveTextContent("no response");
    expect(network).toHaveTextContent(
      "no HTTP response came back, so whether the platform ever saw this read is unknown.",
    );
    expect(network).toHaveAttribute("data-tone", "fail");
    // nothing was asked of a platform that never answered
    expect(network).not.toHaveTextContent("resource");
  });

  it("gives the three states three tones, and every tone its word", async () => {
    const states: ReadonlyArray<{ error: ReaderError; tone: string; word: string }> = [
      {
        error: refused(await fixtureReader.runs.get("01J9NOTHINGSTOREDHEREXX")),
        tone: "neutral",
        word: "not found",
      },
      {
        error: refused(await fixtureReader.runs.get("refused-authorization")),
        tone: "warn",
        word: "refused",
      },
      {
        error: refused(await fixtureReader.runs.get("network-down")),
        tone: "fail",
        word: "no response",
      },
    ];

    for (const state of states) {
      const root = panel(state.error);
      expect(root).toHaveAttribute("data-tone", state.tone);
      expect(root.querySelector(".error-panel-word")).toHaveTextContent(state.word);
      cleanup();
    }
  });
});
