/**
 * The shell reads its org from `config.json` when the page loads.
 */

import { describe, expect, it } from "vitest";

import { envSegment, matchPath } from "../src/address.js";
import { CONFIG_PATH, readAppConfig, readConfig } from "../src/index.js";

function answering(status: number, body: string, path = CONFIG_PATH): typeof fetch {
  return (async (input: RequestInfo | URL) => {
    expect(String(input)).toBe(path);
    return new Response(body, { status, headers: { "content-type": "application/json" } });
  }) as typeof fetch;
}

describe("the config of the page", () => {
  it("reads the org without a project", async () => {
    const config = await readConfig(answering(200, '{"org":"acme"}'));
    expect(config.org).toBe("acme");
    expect(config).not.toHaveProperty("project");
  });

  it("reads the same app configuration from its entry and a deep link", async () => {
    const mount = "/warehouse/receiving/";
    const fetcher = answering(200, '{"org":"acme","project":"warehouse"}', `${mount}config.json`);
    for (const pathname of [mount, `${mount}orders/123/edit`]) {
      await expect(readAppConfig(pathname, fetcher)).resolves.toEqual({ org: "acme", mount });
    }
  });

  it("refuse a missing file", async () => {
    await expect(readConfig(answering(404, "{}"))).rejects.toThrow();
  });

  it("refuse an empty or missing member", async () => {
    await expect(readConfig(answering(200, '{"org":""}'))).rejects.toThrow();
    await expect(readConfig(answering(200, '{"project":"receiving"}'))).rejects.toThrow();
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
