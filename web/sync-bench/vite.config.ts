import solid from "@solidjs/vite-plugin";
import { defineConfig } from "vitest/config";
import { applicationConfig } from "../shell/vite";

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
    plugins: [solid({ performanceTracks: false }), ...application.plugins],
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
