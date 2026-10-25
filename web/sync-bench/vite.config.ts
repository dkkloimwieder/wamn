import { fileURLToPath } from "node:url";
import type { IncomingMessage, ServerResponse } from "node:http";
import solid from "@solidjs/vite-plugin";
import { defineConfig } from "vitest/config";
import { applicationConfig } from "../shell/vite";

/** Serve the live page at the same audience path that an application uses. */
function livePage(request: IncomingMessage, _response: ServerResponse, next: () => void) {
  if (/^\/urn(?:%3A|:)wamn(?:%3A|:)project-env[^/]+\/live\.html(?:\?|$)/i.test(request.url ?? "")) {
    request.url = request.url!.replace(/^\/[^/]+\/live\.html/, "/live.html");
  }
  next();
}

export default defineConfig(() => {
  // The page has no host, so it never takes the dev proxy of `dev.json`.
  const application = applicationConfig({
    root: import.meta.url,
    client: { name: "@wamn/wms-client", path: "../../apps/target/wamn/wamn_wms/client-ts" },
    outDir: "../../target/sync-bench/web",
    port: 5190,
    command: "build",
  });
  return {
    ...application,
    // The measurement page, and the packaging screen on a live host.
    build: {
      ...application.build,
      rollupOptions: {
        input: {
          main: fileURLToPath(new URL("index.html", import.meta.url)),
          live: fileURLToPath(new URL("live.html", import.meta.url)),
        },
      },
    },
    plugins: [
      solid({ performanceTracks: false }),
      ...application.plugins,
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
    resolve: {
      conditions: ["wfe-source", "browser", "development|production"],
      dedupe: ["solid-js", "@solidjs/web", "@solidjs/signals", "@tanstack/db"],
      alias: application.resolve.alias.filter(
        ({ find }) => String(find).includes("wms-client") || find === "@wamn/web-runtime",
      ),
    },
    test: { environment: "jsdom", server: { deps: { inline: true as const } } },
  };
});
