// @vitest-environment node
import type { IncomingMessage, ServerResponse } from "node:http";
import { expect, it, vi } from "vitest";
import { livePage } from "../vite.config.js";

it.each(["live.html", "manifest.webmanifest", "icon.svg"])("serves %s beside the audience page", (file) => {
  for (const audience of ["urn:wamn:project-env:org:project:dev:instance", "urn%3Awamn%3Aproject-env%3Aorg%3Aproject%3Adev%3Ainstance"]) {
    const request = { url: `/${audience}/${file}?v=1` } as IncomingMessage;
    const next = vi.fn();
    livePage(request, {} as ServerResponse, next);
    expect(request.url).toBe(`/${file}?v=1`);
    expect(next).toHaveBeenCalledOnce();
  }
});

it.each(["/password/renew", "/wamn/rows", "/other/live.html", "/urn:wamn:project-env:org:project:dev:instance/icon.svg/extra"])("leaves %s unchanged", (url) => {
  const request = { url } as IncomingMessage;
  livePage(request, {} as ServerResponse, vi.fn());
  expect(request.url).toBe(url);
});
