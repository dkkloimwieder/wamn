/**
 * The shell reads its org from `config.json` when the page loads.
 */

import { describe, expect, it } from "vitest";

import { audienceType } from "../src/address.js";
import { CONFIG_PATH, readConfig } from "../src/index.js";

function answering(status: number, body: string): typeof fetch {
  return (async (input: RequestInfo | URL) => {
    expect(String(input)).toBe(CONFIG_PATH);
    return new Response(body, { status, headers: { "content-type": "application/json" } });
  }) as typeof fetch;
}

describe("the config of the page", () => {
  it("reads the org without a project", async () => {
    await expect(readConfig(answering(200, '{"org":"acme"}'))).resolves.toEqual({
      org: "acme",
    });
  });

  it("does not carry a project filter from the file", async () => {
    await expect(readConfig(answering(200, '{"org":"acme","project":"receiving"}'))).resolves.toEqual({ org: "acme" });
  });

  it("refuse a missing file", async () => {
    await expect(readConfig(answering(404, "{}"))).rejects.toThrow("/config.json answered 404");
  });

  it("refuse an empty or missing member", async () => {
    await expect(readConfig(answering(200, '{"org":""}'))).rejects.toThrow("/config.json has no org");
    await expect(readConfig(answering(200, '{"project":"receiving"}'))).rejects.toThrow("/config.json has no org");
  });
});

describe("audiences of the org-only platform config", () => {
  it("accepts projects in its org and refuses another org", () => {
    const config = { org: "acme" };
    expect(audienceType("urn:wamn:control:acme", config)).toBe("control");
    expect(audienceType("urn:wamn:project-env:acme:receiving:dev:suffix", config)).toBe("application");
    expect(audienceType("urn:wamn:project-env:acme:wms:dev:suffix", config)).toBe("application");
    expect(audienceType("urn:wamn:project-env:other:wms:dev:suffix", config)).toBeUndefined();
    expect(audienceType("urn:wamn:control:other", config)).toBeUndefined();
  });
});
