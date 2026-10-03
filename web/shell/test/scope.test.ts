/**
 * The shell reads its org and project from `config.json` when the page loads.
 */

import { describe, expect, it } from "vitest";

import { SCOPE_PATH, readScope } from "../src/index.js";

function answering(status: number, body: string): typeof fetch {
  return (async (input: RequestInfo | URL) => {
    expect(String(input)).toBe(SCOPE_PATH);
    return new Response(body, { status, headers: { "content-type": "application/json" } });
  }) as typeof fetch;
}

describe("the scope of the page", () => {
  it("read the org and the project", async () => {
    await expect(readScope(answering(200, '{"org":"acme","project":"receiving"}'))).resolves.toEqual({
      org: "acme",
      project: "receiving",
    });
  });

  it("refuse a missing file", async () => {
    await expect(readScope(answering(404, "{}"))).rejects.toThrow("/config.json answered 404");
  });

  it("refuse an empty or missing member", async () => {
    await expect(readScope(answering(200, '{"org":"acme","project":""}'))).rejects.toThrow(
      "/config.json has no project",
    );
    await expect(readScope(answering(200, '{"project":"receiving"}'))).rejects.toThrow("/config.json has no org");
  });
});
