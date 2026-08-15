import { afterEach, describe, expect, it, vi } from "vitest";

import type { AuthoringReader } from "./contract";
import type { ReadResult } from "./errors";
import { fixtureReader } from "./fixture-reader";
import { FAILING_RUN_ID } from "./fixtures";
import { selectReader } from "./reader";

function failure<T>(result: ReadResult<T>): Extract<ReadResult<T>, { ok: false }>["error"] {
  if (result.ok) {
    throw new Error("expected an error");
  }
  return result.error;
}

describe("fixture reader", () => {
  it("structurally satisfies AuthoringReader", () => {
    // The annotation on `fixtureReader` is the compile-time proof; this is the
    // runtime half — the three resources are named and callable.
    const reader: AuthoringReader = fixtureReader;
    expect(typeof reader.runs.get).toBe("function");
    expect(typeof reader.reports.get).toBe("function");
    expect(typeof reader.drafts.get).toBe("function");
  });

  it("returns not-found for an unknown id on every resource", async () => {
    expect(failure(await fixtureReader.runs.get("nope"))).toEqual({
      kind: "not-found",
      literal: "run-not-found",
      resource: "run",
      id: "nope",
      revision: null,
    });
    expect(failure(await fixtureReader.reports.get("nope"))).toEqual({
      kind: "not-found",
      literal: "report-not-found",
      resource: "report",
      id: "nope",
      revision: null,
    });
    expect(failure(await fixtureReader.drafts.get("orders", 99))).toEqual({
      kind: "not-found",
      literal: "draft-revision-not-found",
      resource: "draft",
      id: "orders",
      revision: 99,
    });
  });

  it("returns refusals carrying the platform literal verbatim", async () => {
    expect(failure(await fixtureReader.runs.get("refused-authorization"))).toEqual({
      kind: "refusal",
      literal: "authorization-denied",
      detail: null,
    });
    expect(failure(await fixtureReader.reports.get("refused-contract-version"))).toEqual({
      kind: "refusal",
      literal: "unsupported-contract-version",
      detail: "requested 0.2, supported 0.1",
    });
  });

  it("returns a network error for the reserved id", async () => {
    expect(failure(await fixtureReader.drafts.get("network-down", 1))).toEqual({
      kind: "network",
      message: "authoring request failed before receiving an HTTP response",
    });
  });

  it("resolves a known id rather than probing it", async () => {
    const result = await fixtureReader.runs.get(FAILING_RUN_ID);
    expect(result.ok).toBe(true);
  });
});

describe("dev toggle", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it("reads VITE_READER when called with no argument", () => {
    // Stubbed rather than inherited: otherwise this asserts the runner's
    // environment, and `VITE_READER=http vitest` would fail the default case.
    vi.stubEnv("VITE_READER", undefined);
    expect(selectReader()).toBe(fixtureReader);
    vi.stubEnv("VITE_READER", "http");
    expect(() => selectReader()).toThrow("step 9");
  });

  it("defaults to fixtures for anything but `http`", () => {
    expect(selectReader("fixture")).toBe(fixtureReader);
    expect(selectReader("anything-else")).toBe(fixtureReader);
  });

  it("fails loudly when asked for the reader step 9 has not built", () => {
    expect(() => selectReader("http")).toThrow("step 9");
  });
});
