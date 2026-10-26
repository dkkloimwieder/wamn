import { fileURLToPath } from "node:url";
import type { IncomingMessage, ServerResponse } from "node:http";
import solid from "@solidjs/vite-plugin";
import { defineConfig } from "vitest/config";
import type { Plugin } from "vite";
import { applicationConfig } from "../shell/vite.js";

/** Serve the live page and install assets at the application's audience path. */
export function livePage(request: IncomingMessage, _response: ServerResponse, next: () => void) {
  if (/^\/urn(?:%3A|:)wamn(?:%3A|:)project-env[^/]+\/(?:live\.html|manifest\.webmanifest|icon\.svg)(?:\?|$)/i.test(request.url ?? "")) {
    request.url = request.url!.replace(/^\/[^/]+\//, "/");
  }
  next();
}

/** The @wfe/sync worker recipe, with both bench pages in the manifest. */
function precacheManifest(): Plugin {
  return {
    name: "precache-manifest",
    enforce: "post",
    generateBundle(_options, bundle) {
      const chunk = bundle["sw.js"];
      if (chunk?.type !== "chunk") throw new Error("sw.js was not built");
      const files = Object.keys(bundle).filter((name) => name !== "sw.js");
      // Vite copies public assets separately from the bundle.
      chunk.code = chunk.code.replace("__PRECACHE_MANIFEST__", JSON.stringify(["./", ...files, "manifest.webmanifest", "icon.svg"]));
    },
  };
}

export default defineConfig(() => {
  // The page has no host, so it never takes the dev proxy of `dev.json`.
  const application = applicationConfig({
    root: import.meta.url,
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
          sw: fileURLToPath(new URL("src/sw.ts", import.meta.url)),
        },
        output: { entryFileNames: (chunk) => chunk.name === "sw" ? "sw.js" : "assets/[name]-[hash].js" },
      },
    },
    plugins: [
      solid({ performanceTracks: false }),
      ...application.plugins,
      precacheManifest(),
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
      conditions: ["browser", "development|production"],
      dedupe: ["solid-js", "@solidjs/web", "@solidjs/signals", "@tanstack/db"],
      alias: application.resolve.alias.filter(
        ({ find }) => find === "@wamn/web-runtime",
      ),
    },
    test: { environment: "jsdom", server: { deps: { inline: true as const } } },
  };
});
