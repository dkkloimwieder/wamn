import type { IncomingMessage, ServerResponse } from "node:http";
import { defineConfig } from "vitest/config";
import { applicationConfig, precacheManifest, solidApplication, workerBuild } from "@wamn/vite";

/** Serve the live page and install assets at the application's audience path. */
export function livePage(request: IncomingMessage, _response: ServerResponse, next: () => void) {
  if (
    /^\/urn(?:%3A|:)wamn(?:%3A|:)project-env[^/]+\/(?:live\.html|manifest\.webmanifest|icon\.svg)(?:\?|$)/i.test(
      request.url ?? "",
    )
  ) {
    request.url = request.url!.replace(/^\/[^/]+\//, "/");
  }
  next();
}

export default defineConfig(() => {
  // The page has no host, so it never takes the dev proxy of `dev.json`.
  const application = applicationConfig({
    root: import.meta.url,
    outDir: "../../target/sync-bench/web",
    port: 5190,
    command: "build",
  });
  const solid = solidApplication("sync-bench", ["@tanstack/db"]);
  return {
    ...application,
    ...solid,
    // The measurement page, and the packaging screen on a live host.
    build: {
      ...application.build,
      rollupOptions: workerBuild(import.meta.url, { main: "index.html", live: "live.html" }, "src/sw.ts"),
    },
    plugins: [
      ...solid.plugins,
      ...application.plugins,
      precacheManifest(["manifest.webmanifest", "icon.svg"]),
      {
        name: "packaging-live-page",
        configureServer(server) {
          server.middlewares.use(livePage);
        },
        configurePreviewServer(server) {
          server.middlewares.use(livePage);
        },
      },
    ],
  };
});
