/**
 * The shell reads its org from `config.json` when the page loads.
 */

import { describe, expect, it } from "vitest";

import { audienceKind } from "../src/address.js";
import { SCOPE_PATH, readScope } from "../src/index.js";

function answering(status: number, body: string): typeof fetch {
  return (async (input: RequestInfo | URL) => {
    expect(String(input)).toBe(SCOPE_PATH);
    return new Response(body, { status, headers: { "content-type": "application/json" } });
  }) as typeof fetch;
}

describe("the scope of the page", () => {
  it("reads the org without a project", async () => {
    await expect(readScope(answering(200, '{"org":"acme"}'))).resolves.toEqual({
      org: "acme",
    });
  });

  it("does not carry a project filter from the file", async () => {
    await expect(readScope(answering(200, '{"org":"acme","project":"receiving"}'))).resolves.toEqual({ org: "acme" });
  });

  it("refuse a missing file", async () => {
    await expect(readScope(answering(404, "{}"))).rejects.toThrow("/config.json answered 404");
  });

  it("refuse an empty or missing member", async () => {
    await expect(readScope(answering(200, '{"org":""}'))).rejects.toThrow("/config.json has no org");
    await expect(readScope(answering(200, '{"project":"receiving"}'))).rejects.toThrow("/config.json has no org");
  });
});

describe("audiences of the org-only platform scope", () => {
  it("accepts projects in its org and refuses another org", () => {
    const scope = { org: "acme" };
    expect(audienceKind("urn:wamn:control:acme", scope)).toBe("control");
    expect(audienceKind("urn:wamn:project-env:acme:receiving:dev:suffix", scope)).toBe("application");
    expect(audienceKind("urn:wamn:project-env:acme:wms:dev:suffix", scope)).toBe("application");
    expect(audienceKind("urn:wamn:project-env:other:wms:dev:suffix", scope)).toBeUndefined();
    expect(audienceKind("urn:wamn:control:other", scope)).toBeUndefined();
  });
});
