/**
 * The browser side of the shared classification cases.
 *
 * The table lives beside the rule it mirrors, at
 * `crates/client/tui/tests/data/classification-cases.json`, and
 * `crates/client/tui/tests/classification_table.rs` reads the same file. A case
 * that the two clients read differently is a defect in one of them.
 */

import { describe, expect, it } from "vitest";

import table from "../../../crates/client/tui/tests/data/classification-cases.json" with { type: "json" };
import { classify, createTransport } from "../src/transport.js";
import type { ErrorCase, ResponseContract } from "../src/wire.js";

interface Expectation {
  readonly outcome: string;
  readonly value?: unknown;
  readonly code?: string | null;
  readonly detail?: unknown;
  readonly reason?: string;
  readonly committed_result?: unknown;
  readonly failed_outcome?: unknown;
}

interface Case {
  readonly name: string;
  readonly contract: string;
  readonly request_id: string | null;
  readonly status: number;
  readonly body: string;
  readonly expect: Expectation;
}

function contractOf(name: string): ResponseContract {
  const declared = (table.contracts as { [key: string]: unknown })[name];
  const stated = declared as {
    result_class: string | null;
    partial_schema: string | null;
    errors: ErrorCase[];
    kind: string;
    transaction: string | null;
    direct: boolean;
  };
  return {
    resultClass: stated.result_class,
    partialSchema: stated.partial_schema,
    errors: stated.errors,
    replay: null,
    direct: stated.direct,
    kind: stated.kind,
    transaction: stated.transaction,
  };
}

describe("the shared classification cases", () => {
  const cases = table.cases as unknown as readonly Case[];

  it("covers every branch", () => {
    expect(cases.length).toBeGreaterThanOrEqual(20);
  });

  for (const shared of cases) {
    it(shared.name, () => {
      const outcome = classify(contractOf(shared.contract), shared.request_id, {
        status: shared.status,
        body: shared.body,
      });
      const stated = shared.expect;
      switch (stated.outcome) {
        case "completed":
          expect(outcome).toEqual({ status: "completed", value: stated.value });
          break;
        case "refused":
          expect(outcome).toEqual({
            status: "refused",
            code: stated.code ?? null,
            detail: stated.detail ?? null,
          });
          break;
        case "partially_completed":
          expect(outcome).toEqual({
            status: "partiallyCompleted",
            committedResult: stated.committed_result,
            failedOutcome: stated.failed_outcome,
          });
          break;
        case "uncertain":
          expect(outcome).toEqual({
            status: "uncertain",
            reason: stated.reason,
            retryRefusal: null,
          });
          break;
        default:
          throw new Error(`the table states an unknown outcome: ${stated.outcome}`);
      }
    });
  }
});

describe("a transport failure", () => {
  const contract: ResponseContract = {
    resultClass: "one",
    partialSchema: null,
    errors: [],
    replay: null,
    direct: true,
    kind: "get",
    transaction: "implicit",
  };

  const route = {
    operation: "platform-fixture:widget/get@1.0.0",
    method: "POST",
    template: "/widget/get",
    freshOnly: false,
    contract,
  };

  it("is uncertain, because the request may have run", async () => {
    const transport = createTransport({
      baseUrl: "https://example.test",
      fetch: () => Promise.reject(new Error("connection reset")),
    });
    const outcome = await transport.invoke({
      ...route,
      items: [{ request_id: "r1" }],
    });
    expect(outcome.status).toBe("uncertain");
    if (outcome.status === "uncertain") {
      expect(outcome.reason).toContain("connection reset");
    }
  });

  it("sends the credential and the route the bindings state", async () => {
    let seen: { url: string; init: RequestInit } | null = null;
    const transport = createTransport({
      baseUrl: "https://example.test",
      credential: "token",
      fetch: (url, init) => {
        seen = { url: String(url), init: init ?? {} };
        return Promise.resolve(
          new Response('[{"request_id":"r1","value":{"id":"a"}}]', { status: 200 }),
        );
      },
    });
    const outcome = await transport.invoke({
      ...route,
      items: [{ request_id: "r1" }],
    });
    expect(outcome).toEqual({ status: "completed", value: { id: "a" } });
    const call = seen as { url: string; init: RequestInit } | null;
    expect(call?.url).toBe("https://example.test/widget/get");
    expect(call?.init.method).toBe("POST");
    expect((call?.init.headers as { [name: string]: string })["authorization"]).toBe(
      "Bearer token",
    );
  });
});
