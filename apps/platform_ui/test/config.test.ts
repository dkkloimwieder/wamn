/**
 * The shell reads its org from `config.json` when the page loads.
 */

import { describe, expect, it } from "vitest";

import { audienceType, envSegment, matchPath } from "../src/address.js";
import { CONFIG_PATH, readConfig } from "../src/index.js";

function answering(status: number, body: string): typeof fetch {
  return (async (input: RequestInfo | URL) => {
    expect(String(input)).toBe(CONFIG_PATH);
    return new Response(body, { status, headers: { "content-type": "application/json" } });
  }) as typeof fetch;
}

describe("the config of the page", () => {
  it("reads the org without a project", async () => {
    const config = await readConfig(answering(200, '{"org":"acme"}'));
    expect(config.org).toBe("acme");
    expect(config).not.toHaveProperty("project");
  });

  it("does not carry a project filter from the file", async () => {
    const config = await readConfig(answering(200, '{"org":"acme","project":"receiving"}'));
    expect(config.org).toBe("acme");
    expect(config).not.toHaveProperty("project");
  });

  it("refuse a missing file", async () => {
    await expect(readConfig(answering(404, "{}"))).rejects.toThrow();
  });

  it("refuse an empty or missing member", async () => {
    await expect(readConfig(answering(200, '{"org":""}'))).rejects.toThrow();
    await expect(readConfig(answering(200, '{"project":"receiving"}'))).rejects.toThrow();
  });
});

describe("audiences of the org-only platform config", () => {
  it("accepts projects in its org and refuses another org", () => {
    const config = { org: "acme" };
    expect(audienceType("urn:wamn:org:acme", config)).toBe("control");
    expect(audienceType("urn:wamn:project-env:acme:receiving:dev:suffix", config)).toBe("app");
    expect(audienceType("urn:wamn:project-env:acme:wms:dev:suffix", config)).toBe("app");
    expect(audienceType("urn:wamn:project-env:other:wms:dev:suffix", config)).toBeUndefined();
    expect(audienceType("urn:wamn:org:other", config)).toBeUndefined();
  });
});

it("keeps an environment named envs distinct from the project's list", () => {
  const list = "projects/:project/envs";
  const env = "projects/:project/:env";
  const url = new URL(`https://example.test/projects/shop/${envSegment("envs")}`);
  const path = url.pathname.slice(1);
  expect(matchPath(list, "projects/shop/envs")).toEqual({ project: "shop" });
  expect(matchPath(list, path)).toBeUndefined();
  expect(matchPath(env, path)).toEqual({ project: "shop", env: "envs" });
});
